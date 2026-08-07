import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("structured filters exist alongside the free-text box", () => {
  // Text matches anything on a row; these are exact dimensions an operator
  // thinks in, and one cannot substitute for the other.
  assert.match(html, /id="agent-filter"/);
  assert.match(html, /id="status-filter"/);
  assert.match(main, /function passesFilters\(session\) \{/);
  assert.match(main, /if \(!passesFilters\(session\)\) return false;/);
});

test("grouping never inserts a non-option child into the listbox", () => {
  // The fleet list is role="listbox" with roving tabindex. A header element
  // would break both its ARIA semantics and its keyboard model, so grouping
  // reorders and labels rows instead.
  assert.match(html, /id="fleet-list"[^>]*role="listbox"/);
  assert.doesNotMatch(main, /role="group"/);
  assert.doesNotMatch(main, /createGroupHeader|group-header/);
  assert.match(main, /function groupChip\(session\) \{/);
});

test("a group's position follows its most urgent member", () => {
  // Otherwise the folder holding a session that needs you sinks below one that
  // merely sorts earlier alphabetically.
  const fn = main.slice(main.indexOf("function applyGrouping"));
  const body = fn.slice(0, fn.indexOf("\nfunction syncFilterControls"));
  assert.match(body, /const urgency = \(entries\) => Math\.max\(/);
  assert.match(body, /STATUS_ORDER\[s\.status\] \?\? 0/);
  // Ties fall back to a stable, readable order rather than map order.
  assert.match(body, /left\[0\]\.localeCompare\(right\[0\]\)/);
});

test("grouping is off by default and cycles through every mode", () => {
  assert.match(main, /groupBy: "none",/);
  assert.match(main, /const GROUP_MODES = \["none", "folder", "agent", "status"\];/);
  assert.match(main, /GROUP_MODES\.indexOf\(state\.groupBy\) \+ 1\) % GROUP_MODES\.length/);
});

test("the group button states the mode it is currently in", () => {
  // A cycling control that does not say where it is forces trial and error.
  assert.match(main, /group\.textContent = t\(`group-\$\{state\.groupBy\}`\)/);
  for (const mode of ["none", "folder", "agent", "status"]) {
    assert.ok(new RegExp(`^group-${mode} = `, "m").test(ftl), `group-${mode} missing`);
  }
});

test("every status filter maps to a real status value", () => {
  // A filter naming a status the daemon never emits would silently show
  // nothing, which reads as an empty fleet.
  const block = main.slice(main.indexOf("const STATUS_FILTERS"));
  const body = block.slice(0, block.indexOf("\n};") + 3);
  const meta = main.slice(main.indexOf("const STATUS_META"), main.indexOf("const STATUS_KEYS"));
  for (const status of ["working", "thinking", "idle", "rate-limited", "exited"]) {
    assert.ok(body.includes(`"${status}"`), `${status} not used by any filter`);
    assert.ok(meta.includes(`"${status}"`) || meta.includes(`${status}:`), `${status} unknown`);
  }
});

test("filter and group controls are labelled for assistive tech", () => {
  assert.match(html, /aria-label="Filter by agent"/);
  assert.match(html, /aria-label="Filter by status"/);
  assert.match(html, /id="group-toggle"[^>]*aria-pressed=/);
});
