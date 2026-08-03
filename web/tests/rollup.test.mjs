import assert from "node:assert/strict";
import test from "node:test";

import {
  coverage,
  fleetTotals,
  folderOf,
  formatCost,
  formatTokens,
  isPriced,
  rollupBy,
} from "../src/rollup.js";

const session = (overrides) => ({
  id: "s0001",
  agent: "claude",
  cwd: "C:\\repos\\shop",
  cost_usd: 1,
  tokens: {
    requests: 1,
    input_tokens: 100,
    output_tokens: 10,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
  },
  ...overrides,
});

test("a session with no transcript read is excluded, not counted as zero", () => {
  // Zero is a claim: that the session ran and cost nothing. An unread
  // transcript is not that claim, and folding it in makes the total quietly
  // too low exactly when someone is checking whether it is too high.
  const totals = fleetTotals([
    session({ cost_usd: 2 }),
    session({ id: "s0002", cost_usd: null, tokens: null }),
    session({ id: "s0003", cost_usd: undefined, tokens: undefined }),
  ]);
  assert.equal(totals.cost_usd, 2);
  assert.equal(totals.priced, 1);
  assert.equal(totals.unpriced, 2);
});

test("an unpriced session is never silently absent from the report", () => {
  const totals = fleetTotals([session({ cost_usd: null, tokens: null })]);
  const t = (key, args) => `${key}:${JSON.stringify(args ?? {})}`;
  assert.match(coverage(totals, t), /^rollup-partial:/);
  assert.match(coverage(totals, t), /"unpriced":1/);
});

test("a fully priced fleet still states its coverage", () => {
  // A total with no statement of what it covers reads as the whole fleet.
  const t = (key) => key;
  assert.equal(coverage(fleetTotals([session({})]), t), "rollup-complete");
  assert.equal(coverage(fleetTotals([]), t), "rollup-empty");
});

test("tokens are summed alongside cost, not instead of it", () => {
  // Cost and tokens answer different questions. A run heavy in cache reads and
  // one heavy in output can cost the same and behave nothing alike.
  const totals = fleetTotals([
    session({ tokens: { requests: 2, input_tokens: 5, output_tokens: 7, cache_read_input_tokens: 3, cache_creation_input_tokens: 1 } }),
    session({ id: "s0002", tokens: { requests: 1, input_tokens: 5, output_tokens: 3, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 } }),
  ]);
  assert.equal(totals.requests, 3);
  assert.equal(totals.input_tokens, 10);
  assert.equal(totals.output_tokens, 10);
  assert.equal(totals.cache_read_input_tokens, 3);
  assert.equal(totals.cache_creation_input_tokens, 1);
});

test("a priced session with no token detail still contributes its cost", () => {
  // The two come from the same transcript but a store written by an older
  // version has the cost and not the tokens.
  const totals = fleetTotals([session({ cost_usd: 4, tokens: null })]);
  assert.equal(totals.cost_usd, 4);
  assert.equal(totals.priced, 1);
  assert.equal(totals.input_tokens, 0);
});

test("groups are ordered by what they cost, not by name", () => {
  // The question a rollup answers is "what is expensive", so the answer is at
  // the top rather than wherever the name happens to sort.
  const rows = rollupBy(
    [
      session({ id: "s1", agent: "zeta", cost_usd: 1 }),
      session({ id: "s2", agent: "alpha", cost_usd: 9 }),
    ],
    (item) => item.agent,
  );
  assert.deepEqual(rows.map((row) => row.key), ["alpha", "zeta"]);
});

test("groups that cost the same keep a stable order between renders", () => {
  const rows = rollupBy(
    [session({ id: "s1", agent: "b", cost_usd: 1 }), session({ id: "s2", agent: "a", cost_usd: 1 })],
    (item) => item.agent,
  );
  assert.deepEqual(rows.map((row) => row.key), ["a", "b"]);
});

test("a session with no group value lands in one bucket rather than many", () => {
  const rows = rollupBy([session({ cwd: "" }), session({ id: "s2", cwd: "" })], folderOf);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].priced, 2);
});

test("the folder is the last path component, on either separator", () => {
  assert.equal(folderOf({ cwd: "C:\\repos\\shop" }), "shop");
  assert.equal(folderOf({ cwd: "/home/me/repos/shop/" }), "shop");
  assert.equal(folderOf({}), "—");
});

test("a cost below a cent says so instead of rounding to nothing", () => {
  // $0.00 and "not yet priced" would otherwise look identical, and one of them
  // means the session is running.
  assert.equal(formatCost(0.004), "<$0.01");
  assert.equal(formatCost(0), "$0.00");
  assert.equal(formatCost(null), "—");
  assert.equal(formatCost(undefined), "—");
  assert.equal(formatCost(Number.NaN), "—");
  assert.equal(formatCost(12.345), "$12.35");
});

test("token counts stay narrow enough to sit beside a cost", () => {
  assert.equal(formatTokens(999), "999");
  assert.equal(formatTokens(9999), "9,999");
  assert.equal(formatTokens(45_000), "45k");
  assert.equal(formatTokens(2_500_000), "2.5M");
  assert.equal(formatTokens(undefined), "0");
});

test("isPriced distinguishes an absent figure from a zero one", () => {
  assert.equal(isPriced({ cost_usd: 0 }), true);
  assert.equal(isPriced({ cost_usd: null }), false);
  assert.equal(isPriced({}), false);
  assert.equal(isPriced(null), false);
});
