import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

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
