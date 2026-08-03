import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");

function launcherForm() {
  const dom = new JSDOM(html);
  return { dom, form: dom.window.document.getElementById("launcher-form") };
}

// Launching an agent is unrecoverable: it costs tokens and may write to a repository
// the operator never chose. Nothing in the dialog may reach it except the launch button.
test("no control in the launcher form can submit it", () => {
  const { form } = launcherForm();
  assert.ok(form, "launcher form is present");
  const submitters = Array.from(form.querySelectorAll("button, input[type=submit], input[type=image]"))
    .filter((element) => element.type === "submit" || element.type === "image");
  assert.deepEqual(
    submitters.map((element) => element.id || element.className),
    [],
    "a submit control in the launcher form makes Enter and the close button launch a session",
  );
});

test("closing the launcher is a plain button, not a form submission", () => {
  const { form } = launcherForm();
  const close = form.querySelector(".dialog-close");
  assert.ok(close, "close control is present");
  assert.equal(close.type, "button");
  assert.equal(close.getAttribute("value"), null, "a value attribute implies dialog submission");
  assert.match(main, /\$\("close-launcher-button"\)\.addEventListener\("click", \(\) => \$\("launcher-dialog"\)\.close\(\)\)/);
});

test("implicit submission cannot launch a session", () => {
  const { form } = launcherForm();
  // With no submit button, Enter in a field only submits when exactly one field
  // blocks implicit submission. Assert the form keeps more than one, and that the
  // submit handler refuses regardless.
  const blocking = form.querySelectorAll(
    "input:not([type=checkbox]):not([type=radio]):not([type=button]):not([type=reset]):not([type=submit])",
  );
  assert.ok(blocking.length > 1, "launcher keeps multiple text fields");
  assert.match(main, /\$\("launcher-form"\)\.addEventListener\("submit", \(event\) => event\.preventDefault\(\)\)/);
  assert.match(main, /\$\("launch-button"\)\.addEventListener\("click", \(\) => void launchCurrentSpec\(\)\)/);
  const launchCalls = main.match(/launchCurrentSpec\(\)/g) ?? [];
  assert.equal(launchCalls.length, 2, "launchCurrentSpec is declared once and called from one place");
});

test("launcher catalogs come from runtime capabilities and keep free text", () => {
  assert.doesNotMatch(main, /MODEL_SUGGESTIONS/);
  assert.match(main, /invoke\("agent_capabilities"/);
  assert.match(html, /id="model-input"[^>]+list="model-suggestions"/);
  assert.match(html, /id="effort-input"[^>]+list="effort-suggestions"/);
  assert.match(main, /value\.trim\(\) \|\| null/);
});

test("the focused terminal replaces the placeholder rather than stacking under it", () => {
  const { dom } = launcherForm();
  const host = dom.window.document.getElementById("terminal-host");
  const placeholder = dom.window.document.getElementById("terminal-placeholder");
  assert.ok(host && placeholder);
  assert.equal(placeholder.parentElement, host);
  assert.match(main, /function renderTerminalPlaceholder\(\)/);
  assert.match(main, /\$\("terminal-placeholder"\)\.classList\.toggle\("view-hidden", attached\)/);
  assert.match(main, /function updateTerminalHeader\(\) \{\s*renderTerminalPlaceholder\(\);/);
});
