import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";
import { appSource } from "./appSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = appSource();
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

// Permission-prompt fatigue is the loudest complaint about these agents, and an
// allowlist is the precise lever for it. Every field that carries one has to
// survive a round trip through a preset, or an operator's carefully narrowed
// tool list silently becomes "ask about everything" the next time they launch.
test("the tool, settings, MCP and plugin fields exist and round-trip", () => {
  const { dom } = launcherForm();
  const ids = [
    "allowed-tools-input",
    "disallowed-tools-input",
    "settings-input",
    "setting-sources-input",
    "mcp-config-input",
    "strict-mcp-input",
    "plugin-dirs-input",
    "plugin-urls-input",
  ];
  for (const id of ids) {
    const element = dom.window.document.getElementById(id);
    assert.ok(element, `${id} is missing from the launcher`);
    // Codex expresses none of these, and the core refuses rather than drops
    // them — so the control must disappear when Codex is chosen instead of
    // sitting there offering something that would refuse the launch.
    assert.ok(
      element.closest(".claude-only"),
      `${id} is offered for an agent that cannot express it`,
    );
  }
  const writeSpec = main.slice(main.indexOf("function writeSpec"), main.indexOf("function clearFolderValidation"));
  for (const id of ids) {
    assert.ok(writeSpec.includes(id), `${id} is never restored from a spec`);
  }
  const readSpec = main.slice(main.indexOf("function readSpec"), main.indexOf("function setPermissionValue"));
  for (const id of ids) {
    assert.ok(readSpec.includes(id), `${id} is never read into a spec`);
  }
});

// `--fallback-model` used to sit in that list. It is gone from the launcher
// because `claude --help` restricts it to `--print`, so the control offered a
// behaviour the agent would accept and ignore in every session this tool runs.
// Nothing replaces it: this tool cannot retry a turn on another model itself, so
// the honest answer is not to offer the choice.
test("the launcher offers no control for a flag the agent would ignore", () => {
  const { dom } = launcherForm();
  assert.equal(dom.window.document.getElementById("fallback-model-input"), null);
  const readSpec = main.slice(main.indexOf("function readSpec"), main.indexOf("function setPermissionValue"));
  assert.match(readSpec, /fallback_model: null/);
});

// A plugin URL fetches and runs remote code. The label has to say so where the
// operator is typing, not only in the README.
test("the plugin URL field says what it does before it is used", () => {
  const { dom } = launcherForm();
  const label = dom.window.document.getElementById("plugin-urls-input").closest("label");
  assert.match(label.textContent.toLowerCase(), /remote code/);
  assert.match(label.textContent.toLowerCase(), /http\(s\)/);
});

test("choosing Claude no longer rewrites a plan-mode selection", () => {
  // The reset had been there unchanged since the first Tauri shell commit, with
  // no test and no recorded reason, and it rewrote two of this tool's own
  // built-in presets the moment the launcher synced its fields.
  //
  // Verified against the installed build rather than the documentation before
  // removing it: `claude --help` lists `plan` among the accepted
  // `--permission-mode` choices, and `claude --permission-mode plan --print`
  // runs and exits 0.
  assert.doesNotMatch(
    main,
    /permission-input"\)\.value === "plan"/,
    "the launcher must not rewrite the operator's permission choice",
  );
  const fn = main.slice(main.indexOf("function syncAgentFields"));
  const body = fn.slice(0, fn.indexOf("\n}"));
  assert.doesNotMatch(body, /setPermissionValue/, "syncAgentFields sets no permission");
});

test("plan mode survives a built-in preset into the previewed argv", () => {
  // Two built-in presets carry Permission::Plan, and both agents map it: Claude
  // to `--permission-mode plan`, Codex to a collaboration mode. The mapping is
  // asserted in the Rust that owns it; this pins the fact that the presets
  // still ask for it, so a future edit cannot quietly drop the mode instead.
  const presets = readFileSync(
    new URL("../../crates/terminalai-app/src/preset.rs", import.meta.url),
    "utf8",
  ).replace(/\r\n/g, "\n");
  const planning = presets.match(/permission: Some\(Permission::Plan\)/g) ?? [];
  assert.ok(planning.length >= 2, `expected the planning presets to keep plan mode: ${planning.length}`);
  const launch = readFileSync(
    new URL("../../crates/terminalai-core/src/launch.rs", import.meta.url),
    "utf8",
  ).replace(/\r\n/g, "\n");
  assert.match(launch, /Permission::Plan => "plan"/, "Claude still maps plan to its own flag");
});
