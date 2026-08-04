import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const queue = readFileSync(
  new URL("../../crates/terminalai-core/src/queue.rs", import.meta.url),
  "utf8",
);

const render = (() => {
  const start = main.indexOf("function renderQueue()");
  return main.slice(start, main.indexOf("\nasync function queueRowAction", start));
})();

test("the queue advances on a reported status, never on a timer", () => {
  // A timer would send the next prompt into the middle of a long tool call,
  // where it is ignored or read as an answer to something else.
  const observe = queue.slice(queue.indexOf("pub fn observe"));
  assert.match(observe.slice(0, 1200), /status != SessionStatus::Idle/);
  assert.doesNotMatch(queue, /Duration|Instant|sleep|timeout/);
});

test("an attention state pauses the queue instead of answering it", () => {
  const observe = queue.slice(queue.indexOf("pub fn observe"), queue.indexOf("#[cfg(test)]"));
  assert.match(observe, /SessionStatus::NeedsApproval => \{[\s\S]*?PauseReason::NeedsApproval/);
  assert.match(observe, /SessionStatus::AwaitingInput => \{[\s\S]*?PauseReason::AwaitingInput/);
});

test("every pause reason has a plain-language string", () => {
  // The button says "paused"; the reason is what tells the operator whether
  // the agent is waiting on them or the queue is.
  for (const reason of ["needs_approval", "awaiting_input", "not_running", "operator"]) {
    assert.ok(new RegExp(`^queue-pause-${reason} = `, "m").test(ftl), `${reason} has no string`);
  }
});

test("the row shows how many prompts are waiting", () => {
  const glyph = main.slice(main.indexOf("function queueGlyph"), main.indexOf("function queueTitle"));
  assert.match(glyph, /if \(!count\) return "≡";/);
  assert.match(glyph, /count > 9 \? "9\+" : String\(count\)/);
  assert.match(main, /data-action="queue"/);
});

test("only a paused queue is coloured, because only it needs acting on", () => {
  assert.match(main, /queueButton\.classList\.toggle\("row-action-warn", Boolean\(session\.queue_paused\)\)/);
  assert.match(main, /queueButton\.classList\.toggle\("row-action-active", session\.queued_prompts > 0\)/);
});

test("the dialog always states whether the queue is running or why it is not", () => {
  assert.match(render, /\$\("queue-status"\)\.textContent = paused/);
  assert.match(render, /t\("queue-paused-detail", \{ reason: t\(`queue-pause-\$\{paused\}`\) \}\)/);
  assert.match(render, /: t\("queue-running"\)/);
});

test("pause and resume are never both offered", () => {
  assert.match(render, /\$\("queue-resume-button"\)\.hidden = !paused;/);
  assert.match(render, /\$\("queue-pause-button"\)\.hidden = Boolean\(paused\);/);
});

test("prompts are addressed by id, not by position", () => {
  // The operator is changing the positions, and a reorder that raced a fired
  // prompt would otherwise move the wrong entry.
  assert.match(render, /data-prompt="\$\{escapeHtml\(String\(prompt\.id\)\)\}"/);
  const action = main.slice(main.indexOf("async function queueRowAction"));
  assert.match(action, /const prompt = Number\(row\.dataset\.prompt\);/);
  assert.match(action, /invoke\("reorder_queued_prompt", \{ id, prompt, to:/);
});

test("a prompt can be edited and withdrawn from the dialog", () => {
  assert.match(render, /data-queue-action="save"/);
  assert.match(render, /data-queue-action="remove"/);
  const action = main.slice(main.indexOf("async function queueRowAction"));
  assert.match(action, /invoke\("edit_queued_prompt", \{ id, prompt, text:/);
  assert.match(action, /invoke\("remove_queued_prompt", \{ id, prompt \}\)/);
});

test("an action that raced a fired prompt is reported, not swallowed", () => {
  // The backend names that case; hiding it would look like the click did
  // nothing at all.
  const action = main.slice(main.indexOf("async function queueRowAction"));
  const body = action.slice(0, action.indexOf("\nasync function addQueuedPrompt"));
  assert.match(body, /catch \(error\) \{[\s\S]*?showToast\(String\(error\)\)/);
  assert.match(body, /await refreshQueue\(\);/);
});

test("moving the first prompt up or the last one down does nothing", () => {
  const action = main.slice(main.indexOf("async function queueRowAction"));
  assert.match(action, /action === "up" && index > 0/);
  assert.match(action, /action === "down" && index < state\.queuePrompts\.length - 1/);
});

test("prompt text reaching the DOM is escaped", () => {
  assert.match(render, /escapeHtml\(prompt\.text\)/);
});

test("the dialog is reachable and its controls are wired", () => {
  assert.match(html, /id="queue-dialog"/);
  assert.match(main, /if \(action === "queue"\) await openQueue\(id\);/);
  assert.match(main, /\$\("queue-add-button"\)\.addEventListener\("click", \(\) => void addQueuedPrompt\(\)\)/);
  assert.match(main, /invoke\("pause_queue", \{ id: state\.queueSession \}\)/);
  assert.match(main, /invoke\("resume_queue", \{ id: state\.queueSession \}\)/);
});

test("prompts are fetched when the dialog opens, not carried on every row", () => {
  // A session is re-rendered on each status change, and a prompt can be a
  // quarter of a megabyte.
  assert.match(main, /invoke\("queued_prompts", \{ id \}\)/);
  assert.doesNotMatch(main, /session\.queue_prompts\b/);
  assert.match(main, /session\.queued_prompts/);
});
