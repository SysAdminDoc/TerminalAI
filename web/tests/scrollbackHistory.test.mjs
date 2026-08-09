import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { moduleSource } from "./appSource.mjs";

const main = moduleSource("main.js");
const eventBindings = moduleSource("eventBindings.js");
const terminalHistory = readFileSync(
  new URL("../src/terminalHistory.js", import.meta.url),
  "utf8",
).replace(/\r\n/g, "\n");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

const loadOlderOutput = (() => {
  const start = terminalHistory.indexOf("async function loadOlderOutput");
  return terminalHistory.slice(start, terminalHistory.indexOf("\nasync function attachSessionOutput", start));
})();

test("history is reachable from the terminal toolbar, not only the CLI", () => {
  assert.match(html, /id="terminal-history"/);
  assert.match(eventBindings, /\$\("terminal-history"\)\.addEventListener/);
  assert.ok(/^button-load-history = /m.test(ftl), "button has no localized title");
});

test("fleet row actions are returned from the history boundary before the shell binds them", () => {
  assert.match(terminalHistory, /rowAction,\n  \};/);
  assert.match(main, /rowAction: \(\.\.\.args\) => rowAction\(\.\.\.args\)/);
});

test("history arrives on a channel rather than as a command return value", () => {
  // A Tauri command's return value is JSON, and a byte array encoded as JSON
  // numbers costs several times its own length. The ring already uses a
  // channel for exactly this reason.
  assert.match(loadOlderOutput, /new Channel\(\)/);
  assert.match(loadOlderOutput, /invoke\("stream_scrollback_history"/);
  assert.doesNotMatch(loadOlderOutput, /await invoke\("stream_scrollback_history"\)\s*;/);
});

test("a focus switch mid-read cannot paint one session's history into another", () => {
  // The read is a round trip to the daemon and then to disk. Whatever the
  // operator does in the meantime, the bytes belong to the session they were
  // asked for.
  assert.match(loadOlderOutput, /const generation = state\.focusGeneration;/);
  assert.match(
    loadOlderOutput,
    /if \(state\.focused !== id \|\| state\.focusGeneration !== generation\) return;/,
  );
  assert.match(loadOlderOutput, /writeTerminalBytes\(chunk, id, generation\)/);
});

test("the terminal is reset before history is written, never appended to", () => {
  // xterm cannot insert above existing content, so appending history after the
  // ring would show the older output as though it were the newest.
  const reset = loadOlderOutput.indexOf("state.terminal?.reset()");
  const write = loadOlderOutput.indexOf("writeTerminalBytes(chunk");
  assert.ok(reset > 0 && reset < write, "history written without resetting first");
});

test("an empty history says so instead of blanking the pane", () => {
  // Resetting and then writing nothing would read as the session having
  // produced no output at all.
  assert.match(loadOlderOutput, /if \(!total\) \{/);
  assert.ok(/^history-empty = /m.test(ftl), "no message for a session with no history");
  const empty = loadOlderOutput.indexOf("if (!total)");
  const reset = loadOlderOutput.indexOf("state.terminal?.reset()");
  assert.ok(empty < reset, "the pane is cleared before the empty case is checked");
});

test("a second click while a read is in flight is ignored", () => {
  assert.match(loadOlderOutput, /if \(!id \|\| state\.historyLoading\) return;/);
  assert.match(loadOlderOutput, /state\.historyLoading = true;/);
  assert.match(loadOlderOutput, /finally \{\s*state\.historyLoading = false;/);
});

test("the request stays within one control frame", () => {
  // The response includes the whole in-memory ring plus an older window. The
  // daemon's bounded frame is sized for that request rather than truncating it
  // back to the ring's newest bytes.
  assert.match(terminalHistory, /const MAX_SCROLLBACK_BYTES = 512 \* 1024;/);
  assert.match(terminalHistory, /const HISTORY_OLDER_BYTES = 128 \* 1024;/);
  assert.match(terminalHistory, /const HISTORY_REQUEST_BYTES = MAX_SCROLLBACK_BYTES \+ HISTORY_OLDER_BYTES;/);
  const ring = Number(terminalHistory.match(/const MAX_SCROLLBACK_BYTES = (\d+) \* 1024;/)[1]) * 1024;
  const older = Number(terminalHistory.match(/const HISTORY_OLDER_BYTES = (\d+) \* 1024;/)[1]) * 1024;
  assert.ok(ring + older > ring, "history request must reach before the ring");
  assert.match(loadOlderOutput, /maxBytes: HISTORY_REQUEST_BYTES,/);
});
