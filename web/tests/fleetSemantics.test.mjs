import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { renderFixtureRow } from "./rowFixture.mjs";

import { JSDOM } from "jsdom";
import { appSource } from "./appSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
// The fleet row markup lives in `rowMarkup.js` since it was extracted out of
// this file. These assertions are about what the app renders, not about which
// module holds the template, so they read both.
const main =
  appSource() +
  readFileSync(new URL("../src/rowMarkup.js", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("fleet container and rows use single-select listbox semantics", () => {
  assert.match(html, /id="fleet-list"[^>]*role="listbox"/);
  // Read off the rendered row. The attributes are one contiguous run in the
  // markup whatever the source does with line breaks.
  assert.match(
    renderFixtureRow(),
    /role="option" tabindex="-1" aria-posinset="1" aria-setsize="1" aria-selected="false"/,
  );
  assert.match(main, /aria-posinset/);
  assert.match(main, /aria-setsize/);
  assert.match(main, /aria-selected/);
  assert.match(main, /ArrowDown/);
  assert.match(main, /moveFleetRow/);
});

test("fleet row actions remain explicit buttons and attention glyphs stay distinct", () => {
  assert.match(main, /<button type="button" data-action="pin"/);
  assert.match(main, /<button type="button" data-action="focus"/);
  const approval = main.match(/"needs-approval": \{ glyph: "([^"]+)"/)?.[1];
  const needsYou = main.match(/"needs-you": \{ glyph: "([^"]+)"/)?.[1];
  assert.ok(approval);
  assert.ok(needsYou);
  assert.notEqual(approval, needsYou);
});

test("fleet updates announce actionable transitions and defer priority reordering", () => {
  const summary = html.match(/<div class="fleet-summary"[^>]*>/)?.[0] ?? "";
  assert.doesNotMatch(summary, /aria-live/);
  assert.match(html, /id="fleet-order-notice"[^>]*view-hidden/);
  assert.match(html, /id="apply-fleet-order"/);
  assert.match(main, /state\.announcementTimer = setTimeout\(flushAnnouncements, 2000\)/);
  assert.match(main, /announcementQueue/);
  assert.match(main, /pendingPriorityChanges/);
  assert.match(main, /applyFleetOrder/);
  assert.match(main, /addEventListener\("mouseenter", beginFleetOrderFreeze\)/);
  assert.match(main, /addEventListener\("focusin", beginFleetOrderFreeze\)/);
});

test("preflight remains reachable when daemon state is unavailable", () => {
  assert.match(html, /id="preflight-view"/);
  assert.match(html, /id="preflight-list"/);
  assert.match(html, /id="preflight-toggle"/);
  assert.match(main, /data-preflight-action/);
  assert.match(main, /invoke\("preflight_report"\)/);
  assert.match(main, /invoke\("preflight_fix"/);
  assert.match(main, /state\.preflightMode = true/);
  assert.match(main, /data-diagnostics-action="preflight"/);
});

test("managed hook policy has a distinct blocked and non-fixable state", () => {
  assert.match(main, /blocked: \{ glyph: "⊘", label: "preflight-blocked", tone: "red" \}/);
  assert.match(main, /check\.can_fix \? "" : " disabled"/);
});

test("visibility synchronizers reference elements that exist in the shell", () => {
  const dom = new JSDOM(html);
  for (const functionName of ["syncPreflightVisibility", "syncReviewVisibility"]) {
    const start = main.indexOf(`function ${functionName}()`);
    assert.notEqual(start, -1, `${functionName} is present`);
    const end = main.indexOf("\n}", start);
    assert.notEqual(end, -1, `${functionName} has a body`);
    const body = main.slice(start, end);
    const literal = body.match(/\[([^\]]+)\]\.forEach/);
    assert.ok(literal, `${functionName} has a visibility id list`);
    const ids = [...literal[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
    assert.ok(ids.length > 0, `${functionName} names at least one element`);
    for (const id of ids) {
      assert.ok(dom.window.document.getElementById(id), `${functionName} references missing #${id}`);
    }
  }
});

test("a session the supervisor gave up on does not read like one that finished", () => {
  const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

  // Both terminal phases arrive as status "exited", so the row has to consult
  // the phase; without this it renders one label for a crash loop and a
  // completed job alike.
  assert.match(main, /session\?\.phase === "failed"/);
  assert.match(main, /session\?\.phase === "finished"/);
  assert.match(main, /function lifecycleTone\(/);
  assert.match(main, /function lifecycleDetail\(/);

  // The failure is coloured as one. Grey would say "nothing to see here".
  const tone = main.slice(main.indexOf("function lifecycleTone("));
  assert.match(tone.slice(0, tone.indexOf("\n}")), /phase === "failed"\) return "red"/);

  // Every key the two paths reach exists, with the arguments they pass.
  for (const key of [
    "status-failed",
    "status-finished",
    "status-failed-detail",
    "status-failed-detail-code",
    "status-finished-detail",
  ]) {
    assert.match(ftl, new RegExp(`^${key} =`, "m"), `catalog is missing ${key}`);
  }
  assert.match(ftl, /^status-failed = .*\{ \$restarts \}/m);
  assert.match(ftl, /^status-failed-detail-code = .*\{ \$code \}/m);

  // The reason reaches the row, not only the diagnostics drawer.
  assert.match(main, /class="row-status-label" title="\$\{escapeHtml\(detail \|\| label\)\}"/);
});

test("a stalled session is marked, explained, and sorted above healthy working rows", () => {
  const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
  // The dwell timer was formatted and never thresholded, so the session stuck
  // longest sorted last within Working.
  assert.match(main, /session\?\.stalled\) return t\("status-stalled"/);
  assert.match(main, /const STALL_THRESHOLD_MINUTES = 15;/);
  // The mark carries its own threshold; an unexplained badge is not a signal.
  assert.match(main, /status-stalled-detail", \{ minutes: STALL_THRESHOLD_MINUTES \}/);
  assert.match(ftl, /^status-stalled = /m);
  assert.match(ftl, /^status-stalled-detail = .*\{ \$minutes \}/m);
});

test("a session's memory is shown, and an unsampled one is not shown as zero", () => {
  // A session using nothing and a session we could not measure are different
  // facts; rendering the second as 0 MB would report a healthy number from the
  // absence of a signal.
  assert.match(main, /function memory\(bytes\)/);
  assert.match(main, /if \(!Number\.isFinite\(value\) \|\| value <= 0\) return "—";/);
  assert.match(main, /data-row-memory/);
  assert.match(main, /memoryCell\.classList\.toggle\("row-memory-limited"/);
  for (const key of ["memory-explained", "memory-limited-explained"]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} has no string`);
  }
});
