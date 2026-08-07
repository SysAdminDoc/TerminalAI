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
// it needs no virtual display, no packaged binary and no daemon. Backend
// `invoke` calls reject in a plain browser; that is fine and intended — every
// surface here is opened through the DOM, and the point is how it renders.
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
  { name: "prompt library", kind: "dialog", selector: "#prompt-dialog" },
  { name: "broadcast", kind: "dialog", selector: "#broadcast-dialog" },
  { name: "rollup", kind: "dialog", selector: "#rollup-dialog" },
];

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

/// Open one surface and report what is wrong with it.
///
/// Runs in the page because every question here — what colour did this finally
/// composite to, how wide is this box — is one only a layout engine can answer.
async function auditSurface(page, surface) {
  return page.evaluate(
    ({ surface, CONTRAST_NORMAL, CONTRAST_LARGE }) => {
      const problems = [];
      const root = document.querySelector(surface.selector);
      if (!root) return [`${surface.name}: ${surface.selector} is not in the document`];

      // Reveal it the way the operator would, without needing the backend.
      if (surface.kind === "dialog") {
        if (typeof root.showModal === "function") root.showModal();
        else root.setAttribute("open", "");
      } else if (surface.kind === "view") {
        root.classList.remove("view-hidden");
      } else {
        root.hidden = false;
      }
      if (surface.disclosure) {
        const details = root.querySelector(surface.disclosure);
        if (!details) return [`${surface.name}: ${surface.disclosure} is not in ${surface.selector}`];
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

      // Content must fit its own box, unless the box was explicitly made
      // scrollable — a scrollable container that scrolls is working as designed.
      const overflows = (element) => {
        const style = getComputedStyle(element);
        if (style.overflowX === "auto" || style.overflowX === "scroll") return false;
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
      } else {
        root.hidden = true;
      }
      if (surface.disclosure) {
        const details = root.querySelector(surface.disclosure);
        if (details) details.open = false;
      }
      return problems;
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
        await page.goto(ORIGIN, { waitUntil: "domcontentloaded" });
        // The module boots asynchronously and its backend calls reject; give it
        // a turn to render what it can before measuring.
        await page.waitForTimeout(400);
        for (const surface of SURFACES) {
          const found = await auditSurface(page, surface);
          for (const problem of found) problems.push(`[${colorScheme} ${width}px] ${problem}`);
        }
        console.log(
          `checked ${SURFACES.length} surfaces at ${width}px in ${colorScheme}` +
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
  console.log(`\nall ${SURFACES.length} surfaces clean at ${WIDTHS.join("px, ")}px in ${SCHEMES.join(" and ")}`);
}

await main();
