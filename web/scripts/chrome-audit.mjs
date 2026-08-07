// Drive the real chrome in a real browser.
//
// Every other test under `web/tests/` is `node --test` over jsdom, which parses
// markup and runs script but has no layout engine: it cannot know how wide
// anything is, what colour a rule finally composited to, or whether a panel
// spilled out of its container. The two commits that cut the always-visible
// chrome from 21 controls to 9 and folded seventeen launcher fields behind a
// disclosure are exactly the class of change that reviewing a diff cannot catch.
//
// This is deliberately NOT the WebDriver suite in `run-e2e.mjs`, which is
// blocked on a `DevToolsActivePort` failure and drives the packaged Tauri app.
// This serves the frontend with Vite and drives it with headless Chromium, so
// it needs no virtual display, no packaged binary and no daemon.
//
// Two commands are answered by a stub installed before the app's module runs
// (`installBackendStub`); everything else keeps rejecting as it does with no
// backend. That is the minimum needed to reach the fleet render, and it matters
// because with nothing answering, the preflight call rejects, the app enters
// preflight mode, and `#fleet-list` is `view-hidden` — so for the whole life of
// this script the main view of the application was reported clean without one
// pixel of it being measured. Everything else here is opened through the DOM.
//
// Usage: node scripts/chrome-audit.mjs [--keep-open]
// Exit code is the gate: non-zero means a surface regressed.

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";
import { chromium } from "playwright-core";

const PORT = 5199;
const ORIGIN = `http://127.0.0.1:${PORT}/`;

/// The two widths the 2026-08-03 audit used: the default window, and the width
/// at which the fleet's wide columns start competing for room.
const WIDTHS = [1440, 1100];
const SCHEMES = ["dark", "light"];

/// WCAG AA. 4.5:1 is the same threshold `tests/contrastAndMotion.test.mjs`
/// asserts against the design tokens; this checks what actually composited,
/// which is the half a token test cannot see. Large text is allowed 3:1 by the
/// same standard.
const CONTRAST_NORMAL = 4.5;
const CONTRAST_LARGE = 3;

/// Every surface that is hidden until the operator asks for it. A surface that
/// is never opened is never checked, which is how the launcher disclosure shipped
/// unverified in the first place.
const SURFACES = [
  // A view rather than a dialog, but hidden until asked for all the same, and
  // it carries the landing controls.
  { name: "review view", kind: "view", selector: "#review-view" },
  { name: "app overflow menu", kind: "menu", selector: "#app-menu" },
  { name: "tools overflow menu", kind: "menu", selector: "#tools-menu" },
  { name: "launcher", kind: "dialog", selector: "#launcher-dialog" },
  {
    name: "launcher advanced disclosure",
    kind: "dialog",
    selector: "#launcher-dialog",
    disclosure: ".launcher-advanced",
  },
  { name: "settings", kind: "dialog", selector: "#settings-dialog" },
  { name: "explainer", kind: "dialog", selector: "#explainer-dialog" },
  { name: "queue", kind: "dialog", selector: "#queue-dialog" },
  { name: "projects", kind: "dialog", selector: "#projects-dialog" },
  { name: "history", kind: "dialog", selector: "#history-dialog" },
  { name: "approvals", kind: "dialog", selector: "#approvals-dialog" },
  { name: "fleet search", kind: "dialog", selector: "#search-dialog" },
  { name: "working sets", kind: "dialog", selector: "#working-sets-dialog" },
  { name: "prompt library", kind: "dialog", selector: "#prompt-dialog" },
  { name: "broadcast", kind: "dialog", selector: "#broadcast-dialog" },
  { name: "rollup", kind: "dialog", selector: "#rollup-dialog" },
];

/// The surfaces that only exist once there is a fleet.
///
/// These are already visible when the backend answers, so they are audited in
/// place rather than opened. Until the fixture below existed none of them was
/// ever checked: with no backend the preflight call rejects, `loadPreflight`
/// sets `preflightMode`, and `syncPreflightVisibility` puts `view-hidden` on
/// `#fleet-list`, `#fleet-state-strip` and `#column-labels`. Every element in
/// them therefore failed `isVisible` and was skipped — the audit reported clean
/// on the main view of the application without measuring one pixel of it.
const FIXTURE_SURFACES = [
  { name: "populated fleet", kind: "visible", selector: "#fleet-list" },
  { name: "fleet summary", kind: "visible", selector: "#fleet-summary" },
  { name: "fleet state strip", kind: "visible", selector: "#fleet-state-strip" },
  { name: "fleet column labels", kind: "visible", selector: "#column-labels" },
];

/// The two row densities. Wide is not "the same row, wider" — it un-hides three
/// cells and adds five more, so half of every row's text only exists in it.
const DENSITIES = [
  { name: "compact", wide: false },
  { name: "wide", wide: true },
];

/// Put the fleet list in the requested density, through the real control.
///
/// Clicked rather than set, so the audit exercises the same path the operator
/// does and cannot drift from what the button actually does.
async function setDensity(page, wide) {
  const pressed = await page.getAttribute("#wide-toggle", "aria-pressed");
  if ((pressed === "true") !== wide) {
    await page.click("#wide-toggle");
    await page.waitForFunction(
      (want) => document.querySelector("#fleet-list")?.classList.contains("fleet-list-wide") === want,
      wide,
      { timeout: 5000 },
    );
  }
}

/// How many rows beyond one-per-status the fixture adds, for the variants that
/// are not a status: unread, pinned, focused, a limited memory cell, a full
/// context window and a name long enough to have to elide.
const FIXTURE_VARIANTS = 6;

function startVite() {
  // fileURLToPath, not URL.pathname: on Windows the latter yields "/C:/...",
  // which is not a path any API here accepts.
  const child = spawn(
    process.execPath,
    [
      fileURLToPath(new URL("../node_modules/vite/bin/vite.js", import.meta.url)),
      "--port",
      String(PORT),
      "--strictPort",
    ],
    { cwd: fileURLToPath(new URL("..", import.meta.url)), stdio: ["ignore", "pipe", "pipe"] },
  );
  child.stderr.on("data", (chunk) => process.stderr.write(`vite: ${chunk}`));
  return child;
}

async function waitForServer() {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const response = await fetch(ORIGIN);
      if (response.ok) return;
    } catch {
      // Not listening yet.
    }
    await sleep(200);
  }
  throw new Error(`the dev server never answered on ${ORIGIN}`);
}

/// Stand in for the Tauri backend, in the page, before the app's module runs.
///
/// `@tauri-apps/api` calls straight through to `window.__TAURI_INTERNALS__`, so
/// defining it is the whole mechanism — the app is unmodified and every render
/// path is the real one. Only two commands are answered. Everything else keeps
/// rejecting exactly as it does with no backend at all, because the point is to
/// reach the fleet render, not to simulate a daemon.
///
/// `statuses` is discovered from the running app rather than written here (see
/// `discoverStatuses`), so a status added to the fleet is audited without this
/// file changing.
function installBackendStub(statuses) {
  const nowSecs = Math.floor(Date.now() / 1000);
  const systemTime = (offsetSecs = 0) => ({
    secs_since_epoch: nowSecs + offsetSecs,
    nanos_since_epoch: 0,
  });

  const session = (index, status, overrides = {}) => ({
    id: `s${String(index + 1).padStart(4, "0")}`,
    name: `session-${status}`,
    agent: index % 2 === 0 ? "claude" : "codex",
    status,
    cwd: "C:/repos/terminalai",
    branch: "feature/populated-fleet",
    ports: [42000 + index],
    model: "claude-opus-5",
    effort: "high",
    restarts: index % 3,
    queued_prompts: index % 2,
    tool_progress: { completed: index, total: index + 4 },
    cost_usd: 1.5 + index,
    memory_bytes: 509 * 1024 * 1024,
    status_since: systemTime(-90 * (index + 1)),
    last_line: `wrote crates/terminalai-core/src/registry/ingest.rs — ${status}`,
    context: { used_tokens: 41_000, window_tokens: 200_000, source: "transcript" },
    ...overrides,
  });

  // One row per status the app models, then the variants that are not a
  // status. A rate-limited row carries a real quota so the summary renders its
  // limited branch, which is the banner this fixture exists to reach.
  const sessions = statuses.map((status, index) =>
    session(
      index,
      status,
      status === "rate-limited"
        ? { rate_limit: { scope: "5-hour", resets_at: systemTime(3600) } }
        : {},
    ),
  );
  const base = sessions.length;
  sessions.push(
    session(base, "working", { name: "unread-row", unread: true }),
    session(base + 1, "thinking", { name: "pinned-row", pinned: true }),
    session(base + 2, "idle", { name: "focused-row" }),
    session(base + 3, "working", { name: "memory-limited-row", memory_limited: true }),
    session(base + 4, "thinking", {
      name: "full-context-row",
      context: { used_tokens: 194_000, window_tokens: 200_000, source: "transcript" },
    }),
    // Long enough to have to elide rather than widen the row. A name that
    // pushes the page sideways is precisely what this script exists to catch,
    // and no empty-state audit can produce one.
    session(base + 5, "needs-you", {
      name: "a-very-long-session-name-that-should-elide-instead-of-widening-the-row",
      branch: "feature/an-unusually-long-branch-name-from-a-real-worktree",
      last_line:
        "error[E0432]: unresolved import `crate::registry::ingest::apply_hook_matching` " +
        "in a line long enough to need eliding rather than wrapping the row",
    }),
  );

  const snapshot = {
    sessions,
    focused: sessions.find((entry) => entry.name === "focused-row")?.id ?? null,
    admission: {
      max_live_sessions: 8,
      live_sessions: sessions.length,
      queued_sessions: 2,
      aggregate_cost_usd: sessions.reduce((total, entry) => total + entry.cost_usd, 0),
      dropped_events: 3,
      pricing_version: "audit-fixture",
      pricing_committed: "2026-08-01",
      sessions_reporting_cost: sessions.length,
    },
    store_quarantine: null,
    store_write_error: null,
  };

  // All checks healthy, because an unhealthy preflight hides the entire fleet
  // view — which is how this whole area went unaudited.
  const preflight = {
    checks: [
      { id: "claude", label: "Claude Code", state: "ok", detected: "2.1.170", can_fix: false },
      { id: "codex", label: "Codex", state: "ok", detected: "0.146.0", can_fix: false },
      { id: "hooks", label: "Managed hooks", state: "ok", detected: "installed", can_fix: false },
      { id: "daemon", label: "Daemon", state: "ok", detected: "running", can_fix: false },
      { id: "shortcut", label: "Start menu shortcut", state: "ok", detected: "present", can_fix: false },
    ],
  };

  // The focused row's tone is one of the things this fixture exists to audit,
  // so the attach path has to succeed. It is not optional politeness: a throw
  // anywhere in it is caught by `loadSnapshotNow`, which reports the daemon
  // unavailable and puts the whole fleet view behind `view-hidden` — the rows
  // render and then become unmeasurable.
  const attachPath = [
    "attach_session_output",
    "focus_session",
    "subscribe_output",
    "stream_scrollback",
    "mark_read",
  ];
  const answers = { fleet_snapshot: snapshot, preflight_report: preflight };
  for (const command of attachPath) answers[command] = null;
  let nextCallback = 1;
  window.__TAURI_INTERNALS__ = {
    invoke(command) {
      if (command in answers) return Promise.resolve(answers[command]);
      return Promise.reject(new Error(`audit stub answers no such command: ${command}`));
    },
    transformCallback(callback) {
      const id = nextCallback;
      nextCallback += 1;
      window[`_${id}`] = callback;
      return id;
    },
    unregisterCallback(id) {
      delete window[`_${id}`];
    },
    convertFileSrc: (path) => path,
  };
}

/// Ask the running app which statuses it models.
///
/// The state strip renders one chip per status regardless of how many sessions
/// hold it, and each chip now names its key, so booting once with an empty
/// fleet is enough. Deriving it means the fixture cannot fall behind the app;
/// a hand-written list would silently stop covering a status the day one is
/// added, and report clean for the rest.
async function discoverStatuses(browser) {
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    await page.addInitScript(installBackendStub, []);
    await page.goto(ORIGIN, { waitUntil: "domcontentloaded" });
    // Attached, not visible: with no sessions the empty state hides the strip.
    // Discovery only needs the keys, and requiring visibility here would make
    // the audit depend on a state it is not auditing.
    await page.waitForSelector("#fleet-state-strip [data-status]", {
      state: "attached",
      timeout: 15000,
    });
    const statuses = await page.$$eval("#fleet-state-strip [data-status]", (chips) =>
      chips.map((chip) => chip.dataset.status),
    );
    if (!statuses.length) throw new Error("the state strip named no statuses");
    return statuses;
  } finally {
    await context.close();
  }
}

/// Prove the fixture actually rendered before believing a clean result.
///
/// A fixture that fails to render leaves an empty list, and an empty list has
/// nothing to measure — which reads exactly like a clean audit. This is the
/// same failure the fixture was written to fix, one level up, so it is checked
/// rather than assumed.
async function assertFixtureRendered(page, expectedRows) {
  const seen = await page.evaluate(() => {
    const list = document.querySelector("#fleet-list");
    const rows = list ? [...list.querySelectorAll(".fleet-row")] : [];
    const visible = rows.filter((row) => row.getBoundingClientRect().height > 0);
    return {
      rows: rows.length,
      visible: visible.length,
      hidden: Boolean(list?.classList.contains("view-hidden")),
      unread: rows.filter((row) => row.classList.contains("row-unread")).length,
      focused: rows.filter((row) => row.classList.contains("row-focused")).length,
      summary: (document.querySelector("#fleet-summary")?.textContent ?? "").trim().length,
    };
  });
  const faults = [];
  if (seen.hidden) faults.push("#fleet-list is view-hidden, so nothing in it can be measured");
  if (seen.rows !== expectedRows) faults.push(`rendered ${seen.rows} rows, expected ${expectedRows}`);
  if (seen.visible !== seen.rows) faults.push(`${seen.rows - seen.visible} of ${seen.rows} rows have no height`);
  if (!seen.unread) faults.push("no row carries the unread gradient the fixture asked for");
  if (!seen.focused) faults.push("no row is focused, so the focused tone is unaudited");
  if (!seen.summary) faults.push("#fleet-summary is empty, so the header chips are unaudited");
  if (faults.length) {
    throw new Error(`the populated-fleet fixture did not render: ${faults.join("; ")}`);
  }
}

/// Open one surface and report what is wrong with it.
///
/// Runs in the page because every question here — what colour did this finally
/// composite to, how wide is this box — is one only a layout engine can answer.
async function auditSurface(page, surface) {
  return page.evaluate(
    ({ surface, CONTRAST_NORMAL, CONTRAST_LARGE }) => {
      const problems = [];
      // How many elements actually reached the contrast comparison. A surface
      // that measures nothing reports exactly like a surface that is clean,
      // which is the failure mode this whole script keeps rediscovering, so the
      // count is returned and asserted rather than left to inference.
      let measuredCount = 0;
      const root = document.querySelector(surface.selector);
      if (!root) return {
          problems: [`${surface.name}: ${surface.selector} is not in the document`],
          measuredCount,
        };

      // Reveal it the way the operator would, without needing the backend.
      if (surface.kind === "dialog") {
        if (typeof root.showModal === "function") root.showModal();
        else root.setAttribute("open", "");
      } else if (surface.kind === "view") {
        root.classList.remove("view-hidden");
      } else if (surface.kind === "visible") {
        // Already on screen because the backend answered. Deliberately not
        // revealed: if it is hidden, that is the finding, not something to
        // paper over before measuring.
        if (root.classList.contains("view-hidden") || root.hidden) {
          return {
            problems: [`${surface.name}: ${surface.selector} is hidden, so nothing in it was measured`],
            measuredCount,
          };
        }
      } else {
        root.hidden = false;
      }
      if (surface.disclosure) {
        const details = root.querySelector(surface.disclosure);
        if (!details) {
          return {
            problems: [`${surface.name}: ${surface.disclosure} is not in ${surface.selector}`],
            measuredCount,
          };
        }
        details.open = true;
      }

      const parseColour = (value) => {
        const match = String(value).match(/rgba?\(([^)]+)\)/);
        if (!match) return null;
        const parts = match[1].split(/[\s,/]+/).filter(Boolean).map(Number);
        if (parts.length < 3 || parts.some(Number.isNaN)) return null;
        return { r: parts[0], g: parts[1], b: parts[2], a: parts.length > 3 ? parts[3] : 1 };
      };

      const luminance = ({ r, g, b }) => {
        const channel = (value) => {
          const scaled = value / 255;
          return scaled <= 0.03928 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4;
        };
        return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
      };

      const ratio = (foreground, background) => {
        const [a, b] = [luminance(foreground), luminance(background)];
        const [light, dark] = a > b ? [a, b] : [b, a];
        return (light + 0.05) / (dark + 0.05);
      };

      // What this element is actually painted on.
      //
      // A gradient lives in `background-image` and leaves `background-color`
      // transparent, so reading only the latter walks straight past a primary
      // button and measures its text against the dialog behind it. Every stop
      // is returned and the worst one decides, because the text crosses all of
      // them.
      const ownBackground = (element) => {
        const style = getComputedStyle(element);
        if (style.backgroundImage && style.backgroundImage !== "none") {
          const stops = [...style.backgroundImage.matchAll(/rgba?\([^)]+\)/g)]
            .map((match) => parseColour(match[0]))
            .filter((colour) => colour && colour.a > 0.95);
          if (stops.length) return stops;
        }
        const colour = parseColour(style.backgroundColor);
        return colour && colour.a > 0.95 ? [colour] : null;
      };

      // A transparent background means whatever is behind it, so walk up until
      // something actually paints. Calling the element's own transparent
      // background the backdrop is how a contrast check passes on a colour
      // nobody can see.
      const compositedBackground = (element) => {
        let node = element;
        while (node && node.nodeType === 1) {
          const stops = ownBackground(node);
          if (stops) return stops;
          node = node.parentElement;
        }
        return [{ r: 30, g: 30, b: 46, a: 1 }];
      };

      const isVisible = (element) => {
        const style = getComputedStyle(element);
        if (style.visibility === "hidden" || style.display === "none") return false;
        if (Number(style.opacity) === 0) return false;
        const rect = element.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      };

      // Content must fit its own box, unless the box was explicitly made to
      // handle more than fits. Two ways count:
      //
      //   - it scrolls (`overflow-x: auto|scroll`) — working as designed;
      //   - it elides (`text-overflow: ellipsis`) — also working as designed,
      //     and the whole basis of a 28px row that shows a branch name, a
      //     folder and a line of agent output side by side.
      //
      // Elision is checked because without it this reports every dense cell in
      // every row: `scrollWidth > clientWidth` is the *precondition* for an
      // ellipsis, so a rule that ignores it calls the design a defect 140 times
      // and buries the real findings. What is still reported is the silent
      // case — content cut off by `overflow: hidden` with no ellipsis and no
      // scrollbar, where the operator gets no indication anything was lost.
      const overflows = (element) => {
        const style = getComputedStyle(element);
        if (style.overflowX === "auto" || style.overflowX === "scroll") return false;
        if (style.textOverflow === "ellipsis") return false;
        return element.scrollWidth > element.clientWidth + 1;
      };

      for (const element of [root, ...root.querySelectorAll("*")]) {
        if (!isVisible(element) || element.closest("[hidden]") === element) continue;
        if (overflows(element)) {
          problems.push(
            `${surface.name}: <${element.tagName.toLowerCase()}${element.id ? `#${element.id}` : ""}> ` +
              `overflows its container (${element.scrollWidth}px of content in ${element.clientWidth}px)`,
          );
        }

        // Only elements that actually paint text of their own. A wrapper
        // inherits its children's text in textContent and would be checked
        // against a colour it never renders.
        const ownText = Array.from(element.childNodes)
          .filter((node) => node.nodeType === Node.TEXT_NODE)
          .map((node) => node.textContent.trim())
          .join("");
        if (!ownText) continue;
        if (element.getAttribute("aria-hidden") === "true") continue;

        const style = getComputedStyle(element);
        const foreground = parseColour(style.color);
        if (!foreground || foreground.a < 0.95) continue;
        measuredCount += 1;
        const size = parseFloat(style.fontSize);
        const bold = Number(style.fontWeight) >= 700;
        const threshold = size >= 24 || (bold && size >= 18.66) ? CONTRAST_LARGE : CONTRAST_NORMAL;
        const measured = Math.min(
          ...compositedBackground(element).map((background) => ratio(foreground, background)),
        );
        if (measured < threshold) {
          problems.push(
            `${surface.name}: "${ownText.slice(0, 40)}" is ${measured.toFixed(2)}:1, ` +
              `below ${threshold}:1 (${style.color} on painted background)`,
          );
        }
      }

      // Nothing may push the page sideways. A horizontal scrollbar on the body
      // is the failure this whole script exists to notice.
      if (document.documentElement.scrollWidth > window.innerWidth + 1) {
        problems.push(
          `${surface.name}: the page scrolls horizontally ` +
            `(${document.documentElement.scrollWidth}px in a ${window.innerWidth}px viewport)`,
        );
      }

      if (surface.kind === "dialog") {
        if (typeof root.close === "function") root.close();
        else root.removeAttribute("open");
      } else if (surface.kind === "view") {
        root.classList.add("view-hidden");
      } else if (surface.kind !== "visible") {
        root.hidden = true;
      }
      if (surface.disclosure) {
        const details = root.querySelector(surface.disclosure);
        if (details) details.open = false;
      }
      return { problems, measuredCount };
    },
    { surface, CONTRAST_NORMAL, CONTRAST_LARGE },
  );
}

async function main() {
  const vite = startVite();
  let browser;
  const problems = [];
  try {
    await waitForServer();
    browser = await chromium.launch({ headless: true });
    const statuses = await discoverStatuses(browser);
    const expectedRows = statuses.length + FIXTURE_VARIANTS;
    console.log(`fleet models ${statuses.length} statuses: ${statuses.join(", ")}`);
    const surfaces = [...SURFACES, ...FIXTURE_SURFACES];
    for (const colorScheme of SCHEMES) {
      for (const width of WIDTHS) {
        const context = await browser.newContext({
          colorScheme,
          viewport: { width, height: 900 },
          // The app runs at 125% on this machine; a layout that only holds at
          // 1.0 is a layout that does not hold here.
          deviceScaleFactor: 1.25,
        });
        const page = await context.newPage();
        await page.addInitScript(installBackendStub, statuses);
        await page.goto(ORIGIN, { waitUntil: "domcontentloaded" });
        // The module boots asynchronously; give it a turn to render the fleet
        // before measuring.
        await page.waitForTimeout(600);
        await assertFixtureRendered(page, expectedRows);
        // Both row densities. Compact is the default and wide is a different
        // render, not a wider one: `.fleet-list-wide` is what un-hides the
        // branch, the allocated ports and the status label, and it adds the
        // model/effort/cost/memory/context cells. Auditing only the default
        // leaves five columns and three cells of every row unmeasured, which
        // is what a `display: none` cell looks like to this script — skipped,
        // silently, and indistinguishable from clean.
        for (const density of DENSITIES) {
          await setDensity(page, density.wide);
          for (const surface of surfaces) {
            const { problems: found, measuredCount } = await auditSurface(page, surface);
            const where = `[${colorScheme} ${width}px ${density.name}]`;
            for (const problem of found) problems.push(`${where} ${problem}`);
            // A surface with no measured text was not audited, whatever it
            // reported. Every surface in both lists paints text of its own, so
            // zero is always a fault in the harness rather than a clean result.
            if (!measuredCount) {
              problems.push(`${where} ${surface.name}: measured no text at all, so a clean result here means nothing`);
            }
          }
        }
        console.log(
          `checked ${surfaces.length} surfaces over ${expectedRows} rows in ` +
            `${DENSITIES.length} densities at ${width}px in ${colorScheme}` +
            (problems.length ? "" : " — clean"),
        );
        await context.close();
      }
    }
  } finally {
    if (browser) await browser.close();
    vite.kill();
  }

  if (problems.length) {
    console.error(`\n${problems.length} problem(s):`);
    for (const problem of problems) console.error(`  ${problem}`);
    process.exitCode = 1;
    return;
  }
  console.log(
    `\nall ${SURFACES.length + FIXTURE_SURFACES.length} surfaces clean at ` +
      `${WIDTHS.join("px, ")}px in ${SCHEMES.join(" and ")}, compact and wide`,
  );
}

await main();
