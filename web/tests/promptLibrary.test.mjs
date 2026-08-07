import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

const library = main.slice(
  main.indexOf("function renderPromptLibrary()"),
  main.indexOf("\n/**\n * Run the chosen prompt", main.indexOf("function renderPromptLibrary()")),
);
const roots = main.slice(
  main.indexOf("function renderProjectRoots()"),
  main.indexOf("\nasync function loadKnownProjects", main.indexOf("function renderProjectRoots()")),
);

test("the prompt library exposes stored prompts and editing controls", () => {
  for (const id of [
    "prompts-toggle",
    "prompt-dialog",
    "stored-prompt-list",
    "stored-prompt-name",
    "stored-prompt-text",
    "prompt-new-button",
    "prompt-save-button",
  ]) {
    assert.match(html, new RegExp("id=\"" + id + "\""), id + " is missing");
  }
  for (const key of [
    "prompt-library-title",
    "prompt-library-count",
    "prompt-library-empty",
    "prompt-name-required",
    "prompt-text-required",
    "prompt-saved",
    "prompt-deleted",
    "prompt-not-found",
  ]) {
    assert.match(ftl, new RegExp("^" + key + " = ", "m"), key + " is missing");
  }
});

test("stored prompts can be loaded, saved, renamed, selected, and deleted", () => {
  assert.match(library, /invoke\("list_stored_prompts"\)/);
  assert.match(library, /invoke\("save_stored_prompt",/);
  assert.match(library, /prompt: \{ name, text, source: null \}/);
  assert.match(library, /invoke\("delete_stored_prompt", \{ name: previous \}\)/);
  assert.match(library, /invoke\("delete_stored_prompt", \{ name \}\)/);
  assert.match(library, /data-prompt-select/);
  assert.match(library, /data-prompt-delete/);
  assert.match(main, /\$\("stored-prompt-list"\)\.addEventListener\("click"/);
});

test("prompt names and labels are escaped before reaching the DOM", () => {
  assert.match(library, /escapeHtml\(name\)/);
  assert.match(library, /escapeHtml\(selectLabel\)/);
  assert.match(library, /escapeHtml\(deleteLabel\)/);
  assert.match(library, /escapeHtml\(prompt\.name\)/);
  assert.match(library, /\$\("work-start-button"\)\.disabled = empty;/);
});

test("registered roots have a visible remove affordance", () => {
  assert.match(html, /id="project-root-list"/);
  assert.match(html, /id="project-root-add-button"/);
  assert.match(roots, /invoke\("list_project_roots"\)/);
  assert.match(roots, /invoke\("remove_project_root", \{ path \}\)/);
  assert.match(roots, /data-project-root-remove/);
  assert.match(roots, /escapeHtml\(value\)/);
  assert.match(main, /\$\("project-root-list"\)\.addEventListener\("click"/);
  for (const key of [
    "projects-roots-empty",
    "projects-roots-load-error",
    "projects-root-remove",
    "projects-root-removed",
    "projects-root-not-found",
  ]) {
    assert.match(ftl, new RegExp("^" + key + " = ", "m"), key + " is missing");
  }
});

test("presets have a delete control wired to the backend", () => {
  assert.match(html, /id="delete-preset-button"/);
  assert.match(main, /invoke\("delete_preset", \{ name \}\)/);
  assert.match(main, /\$\("delete-preset-button"\)\.disabled = !\$\("preset-select"\)\.value;/);
  assert.match(ftl, /^preset-deleted = /m);
  assert.match(ftl, /^preset-not-found = /m);
});

test("work queued longer than the deadline is reported as expired, not failed", () => {
  const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
  // Its own outcome category. Reporting it as a failure sends the operator
  // looking for a fault when the fleet was simply busy.
  assert.match(main, /expired: "work-state-expired"/);
  assert.match(main, /expired: counts\.expired \?\? 0/);
  assert.match(main, /state\.kind === "expired"/);
  assert.match(ftl, /^work-outcome = .*\{ \$expired \} expired/m);
  assert.match(ftl, /^work-state-expired = /m);
  assert.match(ftl, /^work-expired-detail = .*\{ \$minutes \}/m);
});
