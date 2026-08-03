import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  hasOpenWork,
  modifiedMillis,
  openItemsCell,
  sortProjects,
  stalenessLabel,
  summarize,
} from "../src/projects.js";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

const t = (key, args) => (args ? `${key}:${JSON.stringify(args)}` : key);
const counted = (open, done = 0) => ({ kind: "counted", open, done });
const at = (millis) => ({
  secs_since_epoch: Math.floor(millis / 1000),
  nanos_since_epoch: (millis % 1000) * 1e6,
});

test("a serde SystemTime is unwrapped, not read as a number", () => {
  // Serde writes it as an object. Reading it as a number yields NaN, which
  // would format as a plausible-looking "today".
  assert.equal(modifiedMillis({ modified: at(1_700_000_000_000) }), 1_700_000_000_000);
  assert.equal(modifiedMillis({ modified: null }), null);
  assert.equal(modifiedMillis({}), null);
  assert.equal(modifiedMillis({ modified: { nanos_since_epoch: 5 } }), null);
});

test("a project with no roadmap reads as unknown, never as zero", () => {
  // Zero claims the project is finished, which sorts it beside one that is —
  // and quietly removes it from consideration.
  const absent = openItemsCell({ state: { kind: "absent" } }, t);
  assert.equal(absent.known, false);
  assert.equal(absent.open, null);
  assert.equal(absent.text, "projects-no-roadmap");
});

test("a roadmap written as prose reads as unreadable, not as empty", () => {
  const prose = openItemsCell({ state: { kind: "no_checklist" } }, t);
  assert.equal(prose.known, false);
  assert.equal(prose.text, "projects-unreadable");
});

test("a counted roadmap shows its number, including a real zero", () => {
  assert.deepEqual(openItemsCell({ state: counted(4) }, t), { known: true, text: "4", open: 4 });
  // A genuine zero is different from unknown and must render as a number.
  assert.deepEqual(openItemsCell({ state: counted(0, 9) }, t), { known: true, text: "0", open: 0 });
});

test("only a counted roadmap with items counts as having work", () => {
  assert.equal(hasOpenWork({ state: counted(1) }), true);
  assert.equal(hasOpenWork({ state: counted(0, 3) }), false);
  assert.equal(hasOpenWork({ state: { kind: "absent" } }), false);
  assert.equal(hasOpenWork({ state: { kind: "no_checklist" } }), false);
  assert.equal(hasOpenWork(undefined), false);
});

test("projects sort by most work first, then most recently touched", () => {
  const now = Date.now();
  const rows = sortProjects(
    [
      { name: "a", roadmap: { state: counted(2), modified: at(now - 10 * 86400000) } },
      { name: "b", roadmap: { state: counted(9), modified: at(now) } },
      { name: "c", roadmap: { state: counted(2), modified: at(now) } },
    ],
    now,
  );
  assert.deepEqual(rows.map((row) => row.name), ["b", "c", "a"]);
});

test("projects whose count is unknown sort after every counted one", () => {
  // They are candidates the operator may still want — just not ones this tool
  // can rank. Sorting them as zero would bury them below finished projects.
  const now = Date.now();
  const rows = sortProjects(
    [
      { name: "unknown", roadmap: { state: { kind: "absent" }, modified: null } },
      { name: "finished", roadmap: { state: counted(0, 5), modified: at(now) } },
      { name: "busy", roadmap: { state: counted(3), modified: at(now) } },
    ],
    now,
  );
  assert.deepEqual(rows.map((row) => row.name), ["busy", "finished", "unknown"]);
});

test("staleness reads in units a person uses", () => {
  const now = Date.now();
  assert.equal(stalenessLabel({ modified: at(now) }, t, now), "touched-today");
  assert.equal(stalenessLabel({ modified: at(now - 3 * 86400000) }, t, now), 'touched-days:{"days":3}');
  assert.match(stalenessLabel({ modified: at(now - 400 * 86400000) }, t, now), /^touched-months:/);
  assert.equal(stalenessLabel({ modified: null }, t, now), null);
});

test("the summary always states how many projects could not be counted", () => {
  // "12 of 300 have open items" reads as a complete survey, and is not one if
  // 200 of those 300 have no roadmap at all.
  const summary = summarize([
    { roadmap: { state: counted(2) } },
    { roadmap: { state: counted(0, 1) } },
    { roadmap: { state: { kind: "absent" } } },
    { roadmap: { state: { kind: "no_checklist" } } },
  ]);
  assert.deepEqual(summary, { withWork: 1, unknown: 2, total: 4 });
  assert.ok(/^projects-summary = /m.test(ftl));
  assert.match(ftl.match(/^projects-summary = .*/m)[0], /\$unknown/);
});

test("the dialog is reachable and its controls are wired", () => {
  assert.match(html, /id="projects-dialog"/);
  assert.match(html, /id="projects-toggle"/);
  assert.match(main, /\$\("projects-toggle"\)\.addEventListener\("click", \(\) => void openProjects\(\)\)/);
  assert.match(main, /\$\("projects-open-only"\)\.addEventListener\("change", \(\) => renderProjects\(\)\)/);
});

test("the dialog opens before the scan rather than after it", () => {
  // Scanning reads a file per project across a few hundred repositories;
  // blocking on that before anything appears reads as the button not working.
  const open = main.slice(main.indexOf("async function openProjects"));
  const body = open.slice(0, open.indexOf("\nfunction createOutputChannel"));
  const shows = body.indexOf("dialog.showModal()");
  const scans = body.indexOf('invoke("scan_projects")');
  assert.ok(shows > 0 && shows < scans, "the scan runs before the dialog is shown");
});

test("launching from a row carries the project's folder into the launcher", () => {
  const render = main.slice(main.indexOf("function renderProjects"));
  const body = render.slice(0, render.indexOf("\nasync function openProjects"));
  assert.match(body, /\$\("cwd-input"\)\.value = button\.dataset\.launchProject;/);
  assert.match(body, /void loadProjectTemplates\(\);/);
});

test("project names, paths and roadmap text reaching the DOM are escaped", () => {
  const render = main.slice(main.indexOf("function renderProjects"));
  // Collapsed, because the formatter wraps long interpolations across lines.
  const body = render.slice(0, render.indexOf("\nasync function openProjects")).replace(/\s+/g, "");
  for (const value of ["item.name", "item.path", "next", "cell.text", "touched"]) {
    assert.ok(body.includes(`escapeHtml(${value})`), `${value} is not escaped`);
  }
});
