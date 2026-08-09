import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registrySource } from "./registrySource.mjs";
import test from "node:test";

import { archivedLabel, folderLabel, renderSessionHistory } from "../src/sessionHistory.js";
import { moduleSource } from "./appSource.mjs";
import { appRustSource } from "./appRustSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const workspacePages = moduleSource("workspacePages.js");
const eventBindings = moduleSource("eventBindings.js");
const app = appRustSource();
const daemon = readFileSync(
  new URL("../../crates/terminalai-daemon/src/lib.rs", import.meta.url),
  "utf8",
);
const registry = registrySource();

const escape = (value) =>
  String(value).replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`);
const translate = (key) => key;
const formatTime = () => "2026-08-04 10:00";
const options = { escape, translate, formatTime };

const archive = (over = {}) => ({
  id: "native-7",
  agent: "claude",
  name: "API rewrite",
  cwd: "C:\\Users\\me\\repos\\TerminalAI",
  command: "claude.exe --model opus",
  archived_at: { secs_since_epoch: 1_770_000_000, nanos_since_epoch: 0 },
  ...over,
});

test("an empty archive says so rather than rendering an empty table", () => {
  const markup = renderSessionHistory([], options);
  assert.match(markup, /session-history-empty/);
  assert.doesNotMatch(markup, /<table/);
});

test("the exact command is shown, because it is the reason the record was kept", () => {
  const markup = renderSessionHistory([archive()], options);
  assert.match(markup, /<code>claude\.exe --model opus<\/code>/);
  assert.match(markup, /API rewrite/);
});

test("a record with no timestamp shows an em dash, not the epoch", () => {
  // Stores written before archives carried a stamp would otherwise date every
  // session to 1970 and look like data rather than a gap.
  assert.equal(archivedLabel(archive({ archived_at: null }), formatTime), "—");
  assert.equal(archivedLabel(archive(), formatTime), "2026-08-04 10:00");
});

test("the folder is shortened to what distinguishes sibling checkouts", () => {
  assert.equal(folderLabel("C:\\Users\\me\\repos\\TerminalAI"), "repos/TerminalAI");
  assert.equal(folderLabel("/home/me/work/api"), "work/api");
  assert.equal(folderLabel(""), "");
});

test("names, folders and commands reaching the DOM are escaped", () => {
  const markup = renderSessionHistory(
    [archive({ name: '<img src=x onerror="alert(1)">', command: "claude.exe --arg <script>" })],
    options,
  );
  assert.doesNotMatch(markup, /<img/);
  assert.doesNotMatch(markup, /<script>/);
});

test("relaunch restores only what the archive actually holds", () => {
  // The command is kept as text. Restoring a model or a sandbox from it would
  // mean parsing an argv this record never promised to keep parseable, so the
  // launcher gets the three fields the archive really carries and the note says
  // so.
  const handler = workspacePages.slice(workspacePages.indexOf("async function openSessionHistory"));
  const body = handler.slice(0, handler.indexOf("\nasync function openProjects"));
  assert.match(body, /\$\("agent-input"\)\.value = archive\.agent;/);
  assert.match(body, /\$\("name-input"\)\.value = archive\.name \?\? "";/);
  assert.match(body, /\$\("cwd-input"\)\.value = archive\.cwd \?\? "";/);
  assert.doesNotMatch(body, /model-input|sandbox-input|permission-input/);
  assert.match(workspacePages, /renderSessionHistory\(archives, \{/);
});

test("a failure to read the history is reported in the dialog, not swallowed", () => {
  const handler = workspacePages.slice(workspacePages.indexOf("async function openSessionHistory"));
  assert.match(handler.slice(0, 1200), /session-history-error/);
});

test("the history request carries no handle the UI could act on", () => {
  // The archive is a read-only record: it must not become a second way to reach
  // a live session.
  assert.match(registry, /pub fn archives\(&self\) -> Vec<ArchivedSession>/);
  assert.match(daemon, /Request::SessionHistory => Response::SessionHistory \{/);
  assert.match(app, /async fn session_history\(/);
});

test("the new command is granted in every capability file", () => {
  for (const file of ["default.json", "wdio.json", "wdio-embedded.json"]) {
    const capability = readFileSync(
      new URL(`../../crates/terminalai-app/capabilities/${file}`, import.meta.url),
      "utf8",
    );
    assert.ok(
      capability.includes("allow-session-history"),
      `${file} does not grant allow-session-history; the ACL is checked at invoke time`,
    );
  }
});

test("the dialog and its toolbar control exist", () => {
  assert.match(html, /id="history-dialog"/);
  assert.match(html, /id="history-toggle"/);
  assert.match(html, /id="history-body"/);
  assert.match(eventBindings, /\$\("history-toggle"\)\.addEventListener\("click", \(\) => void openSessionHistory\(\)\)/);
});
