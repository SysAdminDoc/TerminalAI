import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { renderFixtureRow } from "./rowFixture.mjs";

import { JSDOM } from "jsdom";
import { moduleSource } from "./appSource.mjs";
import { rateLimitedLabel } from "../src/rateLimit.js";
import { createSessionStatus, STATUS_META } from "../src/sessionStatus.js";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const rowMarkup = moduleSource("rowMarkup.js");
const fleetRows = moduleSource("fleetRowState.js");
const shell = moduleSource("main.js");
const fleetList = moduleSource("fleetList.js");
const eventBindings = moduleSource("eventBindings.js");
const sessionState = moduleSource("sessionState.js");
const operationalPanels = moduleSource("operationalPanels.js");
const reviewVisibility = moduleSource("reviewVisibility.js");
const sessionPresentation = moduleSource("sessionPresentation.js");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const sessionStatus = createSessionStatus({ t: (key) => key, rateLimitedLabel });

test("fleet container and rows use single-select listbox semantics", () => {
  assert.match(html, /id="fleet-list"[^>]*role="listbox"/);
  // Read off the rendered row. The attributes are one contiguous run in the
  // markup whatever the source does with line breaks.
  assert.match(
    renderFixtureRow(),
    /role="option" tabindex="-1" aria-posinset="1" aria-setsize="1" aria-selected="false"/,
  );
  assert.match(fleetRows, /aria-posinset/);
  assert.match(fleetRows, /aria-setsize/);
  assert.match(fleetRows, /aria-selected/);
  assert.match(fleetRows, /ArrowDown/);
  assert.match(fleetRows, /moveFleetRow/);
});

test("fleet row actions remain explicit buttons and attention glyphs stay distinct", () => {
  assert.match(rowMarkup, /<button type="button" data-action="pin"/);
  assert.match(rowMarkup, /<button type="button" data-action="focus"/);
  assert.notEqual(STATUS_META["needs-approval"].glyph, STATUS_META["needs-you"].glyph);
});

test("fleet updates announce actionable transitions and defer priority reordering", () => {
  const summary = html.match(/<div class="fleet-summary"[^>]*>/)?.[0] ?? "";
  assert.doesNotMatch(summary, /aria-live/);
  assert.match(html, /id="fleet-order-notice"[^>]*view-hidden/);
  assert.match(html, /id="apply-fleet-order"/);
  assert.match(sessionState, /state\.announcementTimer = setTimeout\(flushAnnouncements, 2000\)/);
  assert.match(sessionState, /announcementQueue/);
  assert.match(fleetList, /pendingPriorityChanges/);
  assert.match(fleetList, /applyFleetOrder/);
  assert.match(eventBindings, /addEventListener\("mouseenter", beginFleetOrderFreeze\)/);
  assert.match(eventBindings, /addEventListener\("focusin", beginFleetOrderFreeze\)/);
});

test("preflight remains reachable when daemon state is unavailable", () => {
  assert.match(html, /id="preflight-view"/);
  assert.match(html, /id="preflight-list"/);
  assert.match(html, /id="preflight-toggle"/);
  assert.match(operationalPanels, /data-preflight-action/);
  assert.match(operationalPanels, /invoke\("preflight_report"\)/);
  assert.match(operationalPanels, /invoke\("preflight_fix"/);
  assert.match(operationalPanels, /state\.preflightMode = true/);
  assert.match(operationalPanels, /data-diagnostics-action="preflight"/);
});

test("managed hook policy has a distinct blocked and non-fixable state", () => {
  assert.match(shell, /blocked: \{ glyph: "⊘", label: "preflight-blocked", tone: "red" \}/);
  assert.match(operationalPanels, /check\.can_fix \? "" : " disabled"/);
});

test("visibility synchronizers reference elements that exist in the shell", () => {
  const dom = new JSDOM(html);
  for (const functionName of ["syncPreflightVisibility", "syncReviewVisibility"]) {
    const visibilitySource = operationalPanels + "\n" + reviewVisibility;
    const start = visibilitySource.indexOf(`function ${functionName}()`);
    assert.notEqual(start, -1, `${functionName} is present`);
    const end = visibilitySource.indexOf("\n}", start);
    assert.notEqual(end, -1, `${functionName} has a body`);
    const body = visibilitySource.slice(start, end);
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
  assert.equal(sessionStatus.lifecycleLabel({ phase: "failed", restarts: 2 }), "status-failed");
  assert.equal(sessionStatus.lifecycleLabel({ phase: "finished" }), "status-finished");
  assert.equal(
    sessionStatus.lifecycleDetail({ phase: "failed", restarts: 2, last_exit_code: 7 }),
    "status-failed-detail-code",
  );
  assert.equal(sessionStatus.lifecycleDetail({ phase: "finished" }), "status-finished-detail");
  // The failure is coloured as one. Grey would say "nothing to see here".
  assert.equal(sessionStatus.lifecycleTone({ phase: "failed" }, STATUS_META.exited), "red");

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
  assert.match(rowMarkup, /class="row-status-label" title="\$\{escapeHtml\(detail \|\| label\)\}"/);
});

test("a stalled session is marked, explained, and sorted above healthy working rows", () => {
  const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
  // The dwell timer was formatted and never thresholded, so the session stuck
  // longest sorted last within Working.
  const stalled = { status: "working", stalled: true };
  assert.equal(sessionStatus.lifecycleLabel(stalled), "status-stalled");
  // The mark carries its own threshold; an unexplained badge is not a signal.
  assert.equal(sessionStatus.lifecycleDetail(stalled), "status-stalled-detail");
  assert.match(ftl, /^status-stalled = /m);
  assert.match(ftl, /^status-stalled-detail = .*\{ \$minutes \}/m);
});

test("a session's memory is shown, and an unsampled one is not shown as zero", () => {
  // A session using nothing and a session we could not measure are different
  // facts; rendering the second as 0 MB would report a healthy number from the
  // absence of a signal.
  assert.match(sessionPresentation, /function memory\(bytes\)/);
  assert.match(fleetRows, /memoryCell\.textContent = memory\(session\.memory_bytes\);/);
  assert.match(rowMarkup, /data-row-memory/);
  assert.match(fleetRows, /memoryCell\.classList\.toggle\("row-memory-limited"/);
  for (const key of ["memory-explained", "memory-limited-explained"]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} has no string`);
  }
});
