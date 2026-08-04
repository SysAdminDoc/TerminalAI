import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");

test("fleet container and rows use single-select listbox semantics", () => {
  assert.match(html, /id="fleet-list"[^>]*role="listbox"/);
  assert.match(main, /role="option" tabindex="-1" aria-posinset="1" aria-setsize="1" aria-selected="false"/);
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
