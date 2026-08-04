import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");

/**
 * Sessions started outside TerminalAI are shown because pretending they do not
 * exist is worse, but the supervisor owns none of them: no pty, no process
 * handle, no ability to answer an approval. The row must therefore never offer
 * an action the daemon cannot perform.
 */
test("the external-session panel exists and is a list, not a listbox", () => {
  const dom = new JSDOM(html);
  const view = dom.window.document.getElementById("external-view");
  const list = dom.window.document.getElementById("external-list");
  assert.ok(view, "external panel is present");
  assert.ok(list, "external list is present");
  assert.equal(list.getAttribute("role"), "list");
  // A listbox implies selection, which implies an action.
  assert.notEqual(list.getAttribute("role"), "listbox");
});

test("external rows carry no actionable control", () => {
  const dom = new JSDOM(html);
  const view = dom.window.document.getElementById("external-view");
  assert.equal(view.querySelectorAll("button, input, select, textarea, a[href]").length, 0);

  // The renderer must not grow one either.
  const renderer = main.match(/function renderExternal\(\)[\s\S]*?\n\}/);
  assert.ok(renderer, "renderExternal must exist");
  assert.doesNotMatch(renderer[0], /<button/, "external rows must not render buttons");
  assert.doesNotMatch(renderer[0], /data-action=/, "external rows must not carry actions");
  assert.doesNotMatch(renderer[0], /role="option"/, "external rows are not selectable");
});

test("a failed lookup never renders as an empty machine", () => {
  // The dominant failure mode in this field is reporting idle from the absence
  // of a signal. An unreadable registry must say so.
  assert.match(main, /state\.externalError = t\("external-load-error"/);
  assert.match(main, /if \(state\.externalError\) \{/);
});

test("ended sessions are dropped and unknown ones are counted, not hidden", () => {
  const renderer = main.match(/function renderExternal\(\)[\s\S]*?\n\}/)[0];
  assert.match(renderer, /session\.state !== "ended"/);
  assert.match(renderer, /session\.state === "unknown"/);
  assert.match(renderer, /unknown/);
});
