import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

const load = (() => {
  const start = main.indexOf("async function loadProjectTemplates");
  return main.slice(start, main.indexOf("\nfunction applyProjectTemplate", start));
})();
const apply = (() => {
  const start = main.indexOf("function applyProjectTemplate");
  return main.slice(start, main.indexOf("\nasync function saveCurrentPreset", start));
})();

test("templates are re-read when the folder changes, not cached", () => {
  // The file is versioned with the repository, so pulling a branch that
  // changes it should change what the launcher offers.
  assert.match(main, /\$\("cwd-input"\)\.addEventListener\("change", \(\) => void loadProjectTemplates\(\)\)/);
  assert.match(main, /void loadProjectTemplates\(\);\s*\n\s*\}\s*\n\s*\}\);/);
  assert.match(load, /invoke\("list_templates", \{ cwd \}\)/);
});

test("a repository with no templates hides the control rather than showing it empty", () => {
  // An empty dropdown claims "none configured yet", which is a different and
  // more distracting statement than not mentioning templates at all.
  assert.match(load, /field\.hidden = state\.templates\.length === 0;/);
  assert.match(html, /class="field field-wide project-template-field" hidden/);
});

test("an unreadable template file is reported, never swallowed", () => {
  // Launching after a silent failure applies the operator's own defaults while
  // they believe the project's were used.
  assert.match(load, /catch \(error\) \{/);
  assert.match(load, /showToast\(t\("template-unreadable"/);
  assert.ok(/^template-unreadable = /m.test(ftl));
});

test("applying a template never changes the chosen folder", () => {
  // The folder is the one choice the operator has already made, and the
  // repository the template came from.
  assert.doesNotMatch(apply, /\$\("cwd-input"\)\.value\s*=/);
});

test("extra directories are resolved under the chosen repository", () => {
  assert.match(apply, /template\.add_dirs \?\? \[\]\)\.map\(\(dir\) => `\$\{cwd\}\/\$\{dir\}`\)/);
});

test("a field the template omits is left alone rather than blanked", () => {
  // A template that sets only a permission mode must not wipe the model the
  // operator typed.
  for (const field of ["agent", "model", "effort", "permission", "sandbox", "profile", "prompt"]) {
    assert.match(
      apply,
      new RegExp(`if \\(template\\.${field}\\) \\$\\("[a-z-]+"\\)\\.value = template\\.${field};`),
      `${field} is applied unconditionally`,
    );
  }
});

test("template names and descriptions reaching the dropdown are escaped", () => {
  // Both come from a file that can arrive with a clone.
  assert.match(load, /escapeHtml\(template\.name\)/);
  assert.match(load, /escapeHtml\(template\.description\)/);
});

test("the applied template is confirmed by name", () => {
  // Otherwise a template that changes four fields at once looks like the form
  // rearranging itself.
  assert.match(apply, /showToast\(t\("template-applied", \{ name: template\.name \}\)/);
  assert.ok(/^template-applied = /m.test(ftl));
});
