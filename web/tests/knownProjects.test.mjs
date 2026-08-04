import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

const load = (() => {
  const start = main.indexOf("async function loadKnownProjects");
  return main.slice(start, main.indexOf("\nasync function registerProjectRoot", start));
})();
const register = (() => {
  const start = main.indexOf("async function registerProjectRoot");
  return main.slice(start, main.indexOf("\nasync function saveCurrentPreset", start));
})();

test("the project list is refreshed every time the launcher opens", () => {
  // A repository cloned five minutes ago must be launchable without telling
  // the app about it.
  assert.match(main, /function openLauncher\(\) \{\s*\n\s*writeSpec\(defaultSpec\(\)\);\s*\n\s*void loadKnownProjects\(\);/);
  assert.match(load, /invoke\("list_projects"\)/);
});

test("with no root registered the dropdown is hidden, not empty", () => {
  // An empty "Known projects" list is a question the operator has no way to
  // answer. The register button is what they see instead.
  assert.match(load, /field\.hidden = state\.projects\.length === 0;/);
  assert.match(load, /\$\("register-root-empty-button"\)\.hidden = state\.projects\.length > 0;/);
  assert.match(html, /class="field field-wide known-projects-field" hidden/);
  assert.match(html, /id="register-root-empty-button"/);
});

test("choosing a project sets the folder and re-reads that project's templates", () => {
  // The folder changed without the input's own change event firing, so the
  // template list would otherwise still describe the previous project.
  const handler = main.slice(main.indexOf('$("project-select").addEventListener'));
  const body = handler.slice(0, handler.indexOf("});") + 3);
  assert.match(body, /\$\("cwd-input"\)\.value = path;/);
  assert.match(body, /void loadProjectTemplates\(\);/);
  assert.match(body, /if \(!path\) return;/);
});

test("registering a root reports how many projects it found", () => {
  // "Registered" alone leaves the operator unable to tell a working root from
  // one pointed at the wrong directory, and that only shows up later as an
  // empty dropdown.
  assert.match(register, /t\("projects-root-added", \{ root, count: found \}\)/);
  assert.match(register, /t\("projects-none-found", \{ root \}\)/);
  assert.match(register, /found \? "success" : ""/);
  for (const key of ["projects-root-added", "projects-none-found"]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} missing`);
  }
});

test("folder-picker failures surface instead of becoming unhandled rejections", () => {
  assert.match(
    register,
    /let root;\s*try \{\s*root = await invoke\("pick_folder"\);[\s\S]*?catch \(error\) \{\s*showToast\(String\(error\)\);\s*return;/,
  );
  const pickers = main.slice(
    main.indexOf('$("pick-folder-button")'),
    main.indexOf('$("save-preset-button")'),
  );
  assert.match(
    pickers,
    /pick-folder-button"\)\.addEventListener\("click", async \(\) => \{\s*let folder;\s*try \{\s*folder = await invoke\("pick_folder"\);[\s\S]*?catch \(error\) \{\s*showToast\(String\(error\)\);\s*return;/,
  );
  assert.match(
    pickers,
    /pick-extra-button"\)\.addEventListener\("click", async \(\) => \{\s*let folders;\s*try \{\s*folders = await invoke\("pick_extra_dirs"\);[\s\S]*?catch \(error\) \{\s*showToast\(String\(error\)\);\s*return;/,
  );
});

test("a refused root is reported and does not clear the list", () => {
  // The backend refuses a root already covered by another; swallowing that
  // would look like a silent no-op.
  assert.match(register, /catch \(error\) \{\s*\n\s*showToast\(String\(error\)\);\s*\n\s*return;/);
});

test("project names and paths reaching the DOM are escaped", () => {
  assert.match(load, /escapeHtml\(project\.path\)/);
  assert.match(load, /escapeHtml\(project\.name\)/);
});

test("the discovery rule stops at a repository rather than descending into it", () => {
  // Otherwise every vendored dependency with a .git becomes a project, and a
  // list of thirty repositories becomes a list of four hundred.
  const project = readFileSync(
    new URL("../../crates/terminalai-core/src/project.rs", import.meta.url),
    "utf8",
  );
  const walk = project.slice(project.indexOf("fn walk("), project.indexOf("pub fn is_repository"));
  assert.match(walk, /if is_repository\(&path\) \{[\s\S]*?continue;\s*\n\s*\}/);
  assert.match(project, /pub const MAX_DEPTH: usize = 2;/);
  assert.match(project, /pub const MAX_PROJECTS: usize = 500;/);
});
