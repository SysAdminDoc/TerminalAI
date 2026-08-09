import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { moduleSource } from "./appSource.mjs";
import { appRustSource } from "./appRustSource.mjs";

const main = moduleSource("workRunPanel.js");
const eventBindings = moduleSource("eventBindings.js");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const app = appRustSource();
const work = readFileSync(new URL("../../crates/terminalai-app/src/work.rs", import.meta.url), "utf8");
const queue = readFileSync(
  new URL("../../crates/terminalai-core/src/work_queue.rs", import.meta.url),
  "utf8",
);

const render = (() => {
  const start = main.indexOf("function renderWorkRun()");
  return main.slice(start, main.indexOf("\n/** Why an entry is in the state it is", start));
})();
const start = (() => {
  const at = main.indexOf("async function startWorkRun()");
  return main.slice(at);
})();

test("a prompt is delivered as a pty write, never as a command-line argument", () => {
  // These prompts are kilobytes of prose containing characters Windows quoting
  // mangles. The session is launched with no initial prompt and the text goes
  // on its prompt queue, which writes it as a bracketed paste.
  const driver = app.slice(app.indexOf("fn drive_work_run"));
  const body = driver.slice(0, driver.indexOf("\n/// Every repository"));
  assert.match(body, /Request::EnqueuePrompt \{/);
  assert.doesNotMatch(body, /initial_prompt/);
  assert.match(body, /cwd: entry\.project\.clone\(\),\s*\n\s*\.\.LaunchSpec::default\(\)/);
});

test("a repository with uncommitted changes is flagged, never launched into", () => {
  // An agent let loose on a dirty tree mixes its work with the operator's, and
  // the diff cannot be separated afterwards.
  const driver = app.slice(app.indexOf("fn drive_work_run"));
  const body = driver.slice(0, driver.indexOf("\n/// Every repository"));
  assert.match(body, /if !tree\.is_clean\(\) \{[\s\S]*?EntryState::Flagged \{ tree \}/);
  // And an unreadable tree counts as not clean.
  assert.match(queue, /ProcessRun::TimedOut => TreeState::Unknown/);
  assert.match(queue, /ProcessRun::Failed \{ detail \} => TreeState::Unknown/);
});

test("the tree is checked when the entry runs, not when the run was created", () => {
  // A tree the operator cleaned up in the meantime should not stay flagged
  // from an hour ago.
  const driver = app.slice(app.indexOf("fn drive_work_run"));
  assert.match(driver.slice(0, 3000), /let tree = terminalai_core::work_queue::tree_state\(&entry\.project\)/);
  assert.match(queue, /Every project starts `Pending`/);
});

test("admission is the fleet's decision, asked one slot at a time", () => {
  // The run must not carry its own copy of "is there room": the slot cap, the
  // spend ceiling and the memory budget all report through one field, so asking
  // for that field is what keeps this loop from enforcing a different set of
  // limits than the gate does.
  const driver = app.slice(app.indexOf("fn drive_work_run"));
  assert.match(driver.slice(0, 2500), /if admission\.admission_block\.is_some\(\) \{\s*\n\s*return Ok\(\(\)\);/);
  assert.doesNotMatch(driver.slice(0, 2500), /admission\.live_sessions >= admission\.max_live_sessions/);
});

test("a run holds on expired credentials instead of failing every project", () => {
  // One expired login would otherwise become one failure per project, none of
  // which says what actually happened.
  const driver = app.slice(app.indexOf("fn drive_work_run"));
  assert.match(driver.slice(0, 2500), /if !admission\.expired_auth\.is_empty\(\) \{\s*\n\s*return Ok\(\(\)\);/);
});

test("a session that started without its prompt is reported, not left running", () => {
  // It exists but has no instruction, which is worse than not starting it.
  const driver = app.slice(app.indexOf("fn drive_work_run"));
  assert.match(driver, /session started but the prompt could not be queued/);
});

test("nothing is invented when the operator's template is missing", () => {
  // A stored prompt named "drain the roadmap" containing something this app
  // made up would be worse than an empty library.
  assert.match(work, /let Ok\(text\) = fs::read_to_string\(&source\) else \{\s*\n\s*continue;/);
  assert.match(work, /research-deep\.txt/);
  assert.match(work, /roadmap-drain\.txt/);
});

test("a deleted seeded prompt is not restored on the next launch", () => {
  assert.match(work, /if state\.seeded \{\s*\n\s*return Ok\(0\);/);
});

test("every outcome category is reported, including the ones that did nothing", () => {
  // A run over forty projects reporting only "done" is one the operator has to
  // audit by hand.
  assert.match(render, /done: counts\.done \?\? 0/);
  for (const key of ["running", "pending", "flagged", "failed", "skipped"]) {
    assert.match(render, new RegExp(`${key}: counts\\.${key} \\?\\? 0`), `${key} not reported`);
  }
  const line = ftl.match(/^work-outcome = .*/m)[0];
  for (const key of ["done", "running", "pending", "flagged", "failed", "skipped"]) {
    assert.ok(line.includes(`$${key}`), `${key} missing from the summary string`);
  }
});

test("only a flagged entry offers a decision", () => {
  assert.match(render, /kind === "flagged"\s*\n?\s*\?/);
  assert.match(render, /data-work-approve=/);
  assert.match(render, /data-work-skip=/);
});

test("every entry state has a plain-language label", () => {
  for (const kind of ["pending", "running", "done", "failed", "skipped", "flagged"]) {
    assert.ok(new RegExp(`^work-state-${kind} = `, "m").test(ftl), `${kind} has no label`);
  }
});

test("the run targets the projects actually listed, not every known one", () => {
  // The filter above the table is how the operator says which they mean; a
  // button that ignored it would launch agents in repositories they had just
  // filtered out. One definition, because the schedule beside the button must
  // not quietly target a different set than the button does.
  const listed = main.slice(main.indexOf("function listedProjects()"));
  const body = listed.slice(0, listed.indexOf("\n}"));
  assert.match(body, /const openOnly = \$\("projects-open-only"\)\.checked;/);
  assert.match(body, /openOnly\s*\n?\s*\? state\.scannedProjects\.filter\(\(item\) => hasOpenWork\(item\.roadmap\)\)/);
  assert.match(start, /const listed = listedProjects\(\);/);
  assert.match(start, /projects: listed\.map\(\(item\) => item\.path\)/);
});

test("with no stored prompts the control is unavailable rather than erroring", () => {
  const load = main.slice(main.indexOf("async function loadStoredPrompts"));
  const body = load.slice(0, load.indexOf("\n/**"));
  assert.match(body, /\$\("work-start-button"\)\.disabled = empty;/);
  assert.ok(/^work-no-prompts = /m.test(ftl));
});

test("the run's controls are wired and its state is refreshed after every action", () => {
  assert.match(html, /id="work-start-button"/);
  assert.match(eventBindings, /\$\("work-start-button"\)\.addEventListener\("click", \(\) => void startWorkRun\(\)\)/);
  assert.match(main, /invoke\("set_work_run_paused", \{ paused \}\)/);
  assert.match(eventBindings, /invoke\("clear_work_run"\)/);
  const action = main.slice(main.indexOf("async function workEntryAction"));
  assert.match(action.slice(0, 400), /await refreshWorkRun\(\);/);
});

test("project names and paths reaching the DOM are escaped", () => {
  assert.match(render, /escapeHtml\(entry\.name\)/);
  assert.match(render, /escapeHtml\(entry\.project\)/);
  assert.match(render, /escapeHtml\(detail\)/);
});
