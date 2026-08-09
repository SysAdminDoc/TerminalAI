// What a row says the memory figure covers, and whether the two ways a row can
// be drawn agree about it.
//
// A fleet row is produced twice by two different code paths: `renderRow` builds
// the markup for a row that did not exist, and the update path patches the cells
// of one that did. That is a standing hazard — a cell whose class or tooltip is
// set in only one of them looks right until a session is updated in place, and
// then shows the *previous* session's answer with no error anywhere. It has
// already happened once, to the cost cell.

import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

import { renderFixtureRow } from "./rowFixture.mjs";
import { moduleSource } from "./appSource.mjs";

const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("the memory cell says how many processes its figure covers", () => {
  // The count is the finding, not decoration: since agent teams a row can be a
  // lead plus several separate agent instances inside one job, and the cap this
  // reading is compared against is enforced over all of them.
  const row = renderFixtureRow({ memory_bytes: 4096, memory_processes: 7 });
  assert.match(row, /data-row-memory title="across 7 processes">4096B<\/b>/);
});

test("a reading that covered no job says so instead of implying a tree of one", () => {
  const row = renderFixtureRow({ memory_bytes: 4096, memory_processes: null });
  assert.match(row, /data-row-memory title="across \? processes"/);
});

test("every string the two cells can render exists in the catalog", () => {
  // Both cells choose between several messages by key. A key with no string
  // renders the key itself, which reads like a bug in the data rather than a
  // missing translation.
  for (const key of [
    "cost-explained",
    "cost-budget-of",
    "cost-budget-spent",
    "memory-explained",
    "memory-limited-explained",
    "memory-unscoped-explained",
  ]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} has no string`);
  }
});

test("the update path sets every cost and memory attribute the full render sets", () => {
  // Read from source because there is no second renderer to diff against: the
  // patch path writes into a DOM the test would have to build by hand, and the
  // failure being guarded is an attribute that one path sets and the other
  // forgets.
  const source = moduleSource("fleetRowState.js");
  const patch = source.slice(
    source.indexOf("wideMeta.querySelector(\"[data-row-model]\")"),
    source.indexOf("const countdown = row.querySelector"),
  );
  assert.notEqual(patch.length, 0, "the update path moved; this test is reading nothing");
  for (const expected of [
    /costCell\.textContent = cost\(/,
    /costCell\.classList\.toggle\("row-budget-spent"/,
    /costCell\.title = costTitle\(/,
    /memoryCell\.classList\.toggle\("row-memory-limited"/,
    /memoryCell\.title = memoryTitle\(/,
  ]) {
    assert.match(patch, expected);
  }
});

test("a row that is a team lead names its teammates, and a solo row shows no team cell", () => {
  // The density argument only holds if a row's cost is legible. Since agent
  // teams a row can be a lead plus several separate agent instances, and the
  // fleet shows that as one line.
  const lead = renderFixtureRow({ teammates: ["reviewer", "tester"] });
  assert.match(lead, /<small>TEAM<\/small><b data-row-team title="[^"]*">reviewer, tester<\/b>/);

  // Absent rather than an em dash: teams are opt-in, so a cell on every row
  // would cost the density it exists to justify while saying nothing.
  for (const solo of [{}, { teammates: [] }, { teammates: null }]) {
    assert.doesNotMatch(renderFixtureRow(solo), /data-row-team/, JSON.stringify(solo));
  }
});

test("teammate names reaching the row are escaped", () => {
  // Read from somebody else's file, so the names are not this tool's to trust.
  const row = renderFixtureRow({ teammates: ['<img src=x onerror="alert(1)">'] });
  assert.doesNotMatch(row, /<img/);
  assert.match(row, /&lt;img/);
});

test("the update path creates and removes the team cell rather than assuming it", () => {
  // Conditional markup: a row that gains a team mid-session has no cell to
  // write into, and one whose team ends must lose the names it had.
  const source = moduleSource("fleetRowState.js");
  const patch = source.slice(
    source.indexOf("const teamNames ="),
    source.indexOf("const memoryCell = wideMeta.querySelector"),
  );
  assert.notEqual(patch.length, 0, "the update path moved; this test is reading nothing");
  assert.match(patch, /existingTeam\?\.closest\("span"\)\?\.remove\(\)/);
  assert.match(patch, /wideMeta\.append\(cell\)/);
});
