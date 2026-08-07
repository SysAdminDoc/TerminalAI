import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";
import { appSource } from "./appSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = appSource();

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

test("an external row shows the agent's own state, not just process liveness", () => {
  // The panel already runs `claude agents --json`, which returns the agent's own
  // status vocabulary, and used to collapse it to "is the pid alive" - so a row
  // read "Running" while the agent had said it was blocked on a permission
  // prompt.
  assert.match(main, /function externalReportedLabel\(/);
  assert.match(main, /session\?\.reported_state/);
  assert.match(main, /session\?\.waiting_for/);
  assert.match(main, /const stateText = reported \? `\$\{metaLabel\(meta\)\} · \$\{reported\}` : metaLabel\(meta\)/);

  // Silence stays silence: no reported state leaves process liveness alone
  // rather than inventing an idle row.
  const start = main.indexOf("function externalReportedLabel(");
  const body = main.slice(start, main.indexOf("\n}", start));
  assert.match(body, /if \(!state\) return "";/);

  const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
  for (const key of ["external-blocked-on", "external-reported-by-agent"]) {
    assert.ok(new RegExp(`^${key} =`, "m").test(ftl), `${key} missing from terminalai.ftl`);
  }
});
