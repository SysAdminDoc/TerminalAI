import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const preset = readFileSync(
  new URL("../../crates/terminalai-app/src/preset.rs", import.meta.url),
  "utf8",
);

const writeSpec = (() => {
  const start = main.indexOf("function writeSpec(spec)");
  return main.slice(start, main.indexOf("\nfunction capabilityForAgent", start));
})();

test("applying a preset that names no folder leaves the folder alone", () => {
  // Built-ins carry no working directory: which configuration and which project
  // are separate choices. Assigning `spec.cwd ?? ""` would blank the folder the
  // operator just chose, and the launch would then have no target at all.
  assert.match(writeSpec, /if \(spec\.cwd\) \$\("cwd-input"\)\.value = spec\.cwd;/);
  assert.doesNotMatch(writeSpec, /\$\("cwd-input"\)\.value = spec\.cwd \?\? ""/);
});

test("built-ins are labelled in the dropdown", () => {
  // An operator who cannot see which presets shipped with the app cannot tell
  // why one of them refuses to be overwritten.
  assert.match(main, /preset\.builtin \? `\$\{preset\.name\} \$\{t\("preset-builtin-mark"\)\}`/);
  assert.ok(/^preset-builtin-mark = /m.test(ftl));
});

test("a preset's description reaches the operator, not just its name", () => {
  assert.match(main, /preset\.description \? ` — \$\{preset\.description\}` : ""/);
  assert.match(main, /title="\$\{escapeHtml\(`\$\{label\}\$\{title\}`\)\}"/);
});

test("preset names and descriptions reaching the DOM are escaped", () => {
  assert.match(main, /escapeHtml\(preset\.name\)/);
  assert.match(main, /value="\$\{escapeHtml\(preset\.name\)\}"/);
});

test("hidden built-ins can be brought back from the UI", () => {
  // Hiding is otherwise a one-way door: a built-in exists only in code, so it
  // cannot be recreated by hand.
  assert.match(html, /id="restore-presets-button"/);
  assert.match(main, /invoke\("restore_builtin_presets"\)/);
  for (const key of ["presets-restored", "presets-none-hidden", "button-restore-presets"]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} missing`);
  }
});

test("restoring nothing says so rather than claiming success", () => {
  const handler = main.slice(main.indexOf('$("restore-presets-button")'));
  const body = handler.slice(0, handler.indexOf("});", handler.indexOf("catch")) + 3);
  assert.match(body, /restored \? t\("presets-restored"[\s\S]*?: t\("presets-none-hidden"\)/);
  assert.match(body, /restored \? "success" : ""/);
});

test("the save path sends the full preset shape the backend expects", () => {
  // A missing field would deserialize to a default and silently claim the
  // operator's preset is built-in.
  assert.match(main, /configured_path: null, builtin: false, description: null/);
});

test("every built-in that never asks permission is isolated", () => {
  // Shipping the dangerous half of that pair without the safe half would be
  // the app recommending it. Asserted here as well as in Rust because this is
  // the claim a reviewer of either side would want to check.
  const list = preset.slice(preset.indexOf("fn builtins()"), preset.indexOf("pub fn is_builtin_name"));
  const bypass = list.split("preset(").filter((chunk) => chunk.includes("Permission::Bypass"));
  assert.ok(bypass.length > 0, "no full-auto preset at all");
  for (const chunk of bypass) {
    assert.match(chunk, /worktree: true/);
  }
});
