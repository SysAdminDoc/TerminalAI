import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

const loadOlderOutput = (() => {
  const start = main.indexOf("async function loadOlderOutput");
  return main.slice(start, main.indexOf("\nasync function attachSessionOutput", start));
})();

test("history is reachable from the terminal toolbar, not only the CLI", () => {
  assert.match(html, /id="terminal-history"/);
  assert.match(main, /\$\("terminal-history"\)\.addEventListener/);
  assert.ok(/^button-load-history = /m.test(ftl), "button has no localized title");
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
  // The daemon clamps to its own ceiling; asking for more would silently
  // return less, which reads as history having been lost.
  assert.match(main, /const HISTORY_REQUEST_BYTES = 128 \* 1024;/);
  assert.match(loadOlderOutput, /maxBytes: HISTORY_REQUEST_BYTES,/);
});
