import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { isRemovable, renderWorktrees, stateLabel } from "../src/worktrees.js";
import { appSource } from "./appSource.mjs";

const main = appSource();
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const worktree = readFileSync(
  new URL("../../crates/terminalai-core/src/worktree.rs", import.meta.url),
  "utf8",
);

const escape = (value) => String(value).replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`);
// Rendered arguments are inlined without quotes, so an assertion on them is not
// also asserting how `escape` treats a quote.
const translate = (key, args) =>
  args ? `${key}(${Object.entries(args).map(([name, value]) => `${name}=${value}`)})` : key;
const options = { escape, translate };

const item = (state, over = {}) => ({
  path: "C:/data/worktrees/shop-s0001",
  repo: "C:/repos/shop",
  branch: "terminalai/s0001",
  state,
  missing_directory: false,
  ...over,
});

test("only a fully merged checkout is offered for removal", () => {
  assert.equal(isRemovable(item({ kind: "merged" })), true);
  assert.equal(isRemovable(item({ kind: "unmerged", commits: 3 })), false);
  assert.equal(isRemovable(item({ kind: "unknown", detail: "git timed out" })), false);
});

test("a branch holding work is listed with its count and no control", () => {
  // This view cannot show what those commits contain, so offering to delete
  // them here would ask for a decision on evidence it has not presented.
  const markup = renderWorktrees([item({ kind: "unmerged", commits: 4 })], options);
  assert.match(markup, /terminalai\/s0001/);
  assert.match(markup, /commits=4/);
  assert.doesNotMatch(markup, /data-reap=/);
});

test("an unknown state gets no control either", () => {
  const markup = renderWorktrees([item({ kind: "unknown", detail: "no such branch" })], options);
  assert.doesNotMatch(markup, /data-reap=/);
});

test("a merged checkout gets exactly one control", () => {
  const markup = renderWorktrees([item({ kind: "merged" })], options);
  assert.equal((markup.match(/data-reap=/g) ?? []).length, 1);
});

test("a registration whose directory is gone says so", () => {
  // It is the case that makes every later `git worktree add` for that path
  // fail, so it must be distinguishable from an ordinary leftover.
  const markup = renderWorktrees([item({ kind: "merged" }, { missing_directory: true })], options);
  assert.match(markup, /worktrees-missing-directory/);
});

test("an empty survey says so rather than rendering an empty table", () => {
  const markup = renderWorktrees([], options);
  assert.match(markup, /worktrees-empty/);
  assert.doesNotMatch(markup, /<table/);
});

test("branch names and paths reaching the DOM are escaped", () => {
  const markup = renderWorktrees(
    [item({ kind: "merged" }, { branch: "<img src=x onerror=alert(1)>" })],
    options,
  );
  assert.doesNotMatch(markup, /<img/);
});

test("every state has a label", () => {
  assert.equal(stateLabel({ kind: "merged" }, translate), "worktrees-state-merged");
  assert.match(stateLabel({ kind: "unmerged", commits: 2 }, translate), /^worktrees-state-unmerged/);
  assert.match(stateLabel(undefined, translate), /^worktrees-state-unknown/);
});

test("the refusal lives in the core, not only in this view", () => {
  // A caller that skipped the window would otherwise delete commits.
  assert.match(worktree, /pub fn reap\(stale: &StaleWorktree\) -> Result<\(\), Vec<String>>/);
  assert.match(worktree, /if !stale\.is_safe_to_remove\(\) \{/);
  // And nothing outside this tool's own branch prefix is ever surveyed.
  assert.match(worktree, /if !branch\.starts_with\(BRANCH_PREFIX\) \{\s*\n\s*return None;/);
});

test("the survey is refreshed after a removal rather than patched in place", () => {
  const handler = main.slice(main.indexOf("async function refreshWorktrees"));
  const body = handler.slice(0, handler.indexOf("\n/** Survey leftover"));
  assert.match(handler.slice(0, 2000), /await refreshWorktrees\(\);/);
  void body;
  assert.match(html, /id="worktrees-body"/);
  assert.match(html, /id="worktrees-count"/);
});
