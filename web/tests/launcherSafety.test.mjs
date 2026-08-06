import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

function launcherForm() {
  const dom = new JSDOM(html);
  return { dom, form: dom.window.document.getElementById("launcher-form") };
}

test("the launcher has an accessible name and anchored folder validation", () => {
  const { dom } = launcherForm();
  const dialog = dom.window.document.getElementById("launcher-dialog");
  const input = dom.window.document.getElementById("cwd-input");
  const error = dom.window.document.getElementById("cwd-error");
  assert.equal(dialog.getAttribute("aria-label"), "Launch an agent");
  assert.equal(dialog.getAttribute("data-i18n-aria-label"), "launcher-title");
  assert.equal(input.getAttribute("aria-describedby"), "cwd-error");
  assert.equal(error.getAttribute("role"), "alert");
  assert.equal(error.hidden, true);
  assert.match(html, /placeholder="C:\\Users\\me\\repos\\project"/);
  assert.doesNotMatch(html, /placeholder="C:\\\\Users\\me\\repos\\project"/);
  assert.match(ftl, /^launcher-title = /m);
  assert.match(ftl, /^launcher-folder-required = /m);
});

test("an empty folder focuses the field and keeps the failure beside it", () => {
  const launch = main.slice(main.indexOf("async function launchCurrentSpec"), main.indexOf("async function loadPresets"));
  assert.match(launch, /showFolderValidation\(\)/);
  assert.doesNotMatch(launch, /showToast\("Choose a project folder first"\)/);
  assert.match(main, /input\.setAttribute\("aria-invalid", "true"\)/);
  assert.match(main, /message\.hidden = false;/);
  assert.match(main, /input\.focus\(\);/);
  assert.match(main, /input\.setCustomValidity\(text\)/);
  assert.match(main, /if \(id === "cwd-input"\) clearFolderValidation\(\)/);
});

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

test("stale preview responses are discarded after a newer edit", () => {
  const preview = main.slice(main.indexOf("function schedulePreview"), main.indexOf("async function launchCurrentSpec"));
  assert.match(main, /previewRequest: 0/);
  assert.match(preview, /const request = \+\+state\.previewRequest/);
  assert.match(preview, /setTimeout\(\(\) => updatePreview\(request\), 180\)/);
  assert.match(
    preview,
    /const command = await invoke\("preview_launch", invokeArgs\(spec\)\);\s*if \(request !== state\.previewRequest\) return;/,
  );
  assert.match(
    preview,
    /catch \(error\) \{\s*if \(request !== state\.previewRequest\) return;/,
  );
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

test("a permission mode this build does not model survives a round trip", () => {
  // The core keeps an unmodelled mode as Permission::Custom. A <select> has no
  // option for it and would silently reduce it to "", launching with no mode at
  // all — so the value is carried into the list rather than dropped.
  const { dom } = launcherForm();
  const select = dom.window.document.getElementById("permission-input");
  const modelled = Array.from(select.options).map((option) => option.value);
  assert.deepEqual(modelled, ["ask", "plan", "accept-edits", "bypass"]);

  select.value = "dontAsk";
  assert.equal(select.value, "", "a bare assignment is exactly what must not be relied on");

  assert.match(main, /function setPermissionValue\(value\) \{/);
  assert.match(main, /dataset\.passthrough === "true"\) option\.remove\(\)/);
  assert.match(main, /t\("launcher-permission-custom", \{ mode: wanted \}\)/);
  assert.match(ftl, /^launcher-permission-custom = /m);
});

test("every path that writes a permission mode goes through the setter", () => {
  // writeSpec (presets, resumed specs) and applyProjectTemplate both write it.
  // A bare assignment on either would reintroduce the silent drop.
  assert.doesNotMatch(main, /\$\("permission-input"\)\.value = /);
});

test("the credential fields default to inheriting nothing", () => {
  // The core refuses a name that is unset, so an empty text box must produce an
  // empty list rather than one entry that is the empty string.
  const { dom } = launcherForm();
  const home = dom.window.document.getElementById("agent-home-input");
  const passthrough = dom.window.document.getElementById("env-passthrough-input");
  assert.equal(home.value, "", "no config directory is the signed-in account");
  assert.equal(passthrough.value, "", "nothing is inherited unless named");

  const readSpec = main.slice(main.indexOf("function readSpec()"), main.indexOf("function setPermissionValue"));
  assert.match(readSpec, /agent_home: \$\("agent-home-input"\)\.value\.trim\(\) \|\| null/);
  assert.match(readSpec, /\.map\(\(name\) => name\.trim\(\)\)\s*\n\s*\.filter\(Boolean\)/);
});
