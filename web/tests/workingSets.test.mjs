import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

import {
  renderRestoreOutcomes,
  renderWorkingSet,
  summarizeRestore,
} from "../src/workingSets.js";
import { moduleSource } from "./appSource.mjs";

const main = moduleSource("workspacePages.js");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const rust = readFileSync(
  new URL("../../crates/terminalai-app/src/workingset.rs", import.meta.url),
  "utf8",
);

const escapeHtml = (value) =>
  String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
const options = { escape: escapeHtml, translate: (key) => key };

test("a queued session is not counted as a refusal", () => {
  // The admission gate holding a session back is the gate working. Calling it
  // a failure would train the operator to raise the limit.
  const summary = summarizeRestore([
    { id: "s1", queued: false },
    { id: "s2", queued: true },
    { id: null, refused: "the working tree has uncommitted changes" },
  ]);
  assert.deepEqual(summary, { started: 1, queued: 1, refused: 1 });
});

test("each refusal is quoted rather than summarised away", () => {
  // Twelve sessions can be refused for several different reasons, and the
  // reason is the whole content of the report — "3 refused" is not actionable.
  const markup = renderRestoreOutcomes(
    [
      { name: "api", queued: false, id: "s1" },
      { name: "web", refused: "the working tree has uncommitted changes" },
      { name: "docs", queued: true, id: "s3" },
    ],
    options,
  );
  assert.match(markup, /the working tree has uncommitted changes/);
  assert.match(markup, /class="restore-refused"/);
  assert.match(markup, /class="restore-started"/);
  assert.match(markup, /working-sets-queued/);
});

test("a session that started without its pin says so beside itself", () => {
  // The session did start. The only thing the operator needs to know is that
  // it came back without a live grid, which belongs on that row and not in a
  // separate failure list.
  const markup = renderRestoreOutcomes(
    [{ name: "api", id: "s1", pin_refused: "the fleet holds at most three pinned sessions" }],
    options,
  );
  assert.match(markup, /class="restore-started"/);
  assert.match(markup, /restore-pin/);
  assert.match(markup, /at most three pinned sessions/);
});

test("a layout name cannot inject markup", () => {
  // The name is operator input and reaches both an attribute and a text node.
  const hostile = '"><img src=x onerror="alert(1)">';
  const markup = renderWorkingSet(
    { name: hostile, members: [{ pinned: true }, { pinned: false }] },
    options,
  );
  const dom = new JSDOM(`<body>${markup}</body>`);
  assert.equal(dom.window.document.querySelectorAll("img").length, 0);
  assert.equal(
    dom.window.document.querySelector("[data-restore-set]").dataset.restoreSet,
    hostile,
    "the name survives intact in the attribute",
  );
});

test("the member count reports how many were pinned", () => {
  const markup = renderWorkingSet(
    { name: "morning", members: [{ pinned: true }, { pinned: true }, { pinned: false }] },
    { escape: escapeHtml, translate: (key, args) => `${key}:${JSON.stringify(args)}` },
  );
  // Read back as text, not as markup: the escaper turns the quotes in the
  // rendered args into entities, which is correct and not what is being tested.
  const dom = new JSDOM(`<body>${markup}</body>`);
  assert.equal(
    dom.window.document.querySelector(".search-result-count").textContent,
    'working-sets-members:{"count":3,"pinned":2}',
  );
});

test("restoring goes through the ordinary launch path, not a second one", () => {
  // The whole safety argument. A bespoke restore path would bypass admission,
  // the memory budget, the spend ceiling and the dirty-tree refusal — and it
  // would bypass the next limit added, silently, on the day it is added.
  assert.match(rust, /Restoring is not a second launch path/);
  const command = main.indexOf("async function restoreWorkingSet");
  assert.ok(command > 0, "the renderer has a restore entry point");
  // The renderer never assembles a launch: it names a saved layout and reads
  // back what the fleet decided.
  const body = main.slice(command, main.indexOf("/// Ask the daemon which sessions printed"));
  assert.match(body, /invoke\("restore_working_set", \{ name \}\)/);
  assert.doesNotMatch(body, /invoke\("launch_session"/);
});

test("a layout never carries a worktree path or branch to adopt", () => {
  // Two sessions sharing one private checkout is the failure the worktree
  // feature exists to refuse, and a saved layout is the easiest way to arrange
  // it by accident. The member shape has no field that could.
  const member = rust.slice(rust.indexOf("pub struct WorkingSetMember"));
  const fields = member.slice(0, member.indexOf("}"));
  assert.doesNotMatch(fields, /worktree_path|branch/);
  assert.match(rust, /never adopts the\n\/\/! checkout the original session had/);
});

test("the layouts dialog exists and announces its count", () => {
  assert.match(html, /id="working-sets-dialog"/);
  assert.match(html, /id="working-sets-count"[^>]*role="status"[^>]*aria-live="polite"/);
  assert.match(html, /id="working-set-save"/);
});
