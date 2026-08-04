import test from "node:test";
import assert from "node:assert/strict";

import { spendCeiling, spendCeilingTitle } from "../src/spendCeiling.js";

/** Enough of the real translator to see which message was chosen. */
const t = (key, args = {}) =>
  `${key}${Object.keys(args).length ? `(${Object.entries(args).map(([k, v]) => `${k}=${v}`).join(",")})` : ""}`;

test("no configured ceiling renders nothing rather than a ceiling of zero", () => {
  assert.equal(spendCeiling({ spend_window_usd: 12 }), null);
  assert.equal(spendCeiling({ spend_ceiling_usd: null, spend_window_usd: 12 }), null);
});

test("a ceiling reports how much of it is used", () => {
  const info = spendCeiling({
    spend_ceiling_usd: 20,
    spend_window_usd: 5,
    spend_window_hours: 24,
  });
  assert.equal(info.ceiling, 20);
  assert.equal(info.spent, 5);
  assert.equal(info.percent, 25);
  assert.equal(info.hours, 24);
  assert.equal(info.blocked, false);
});

test("the ceiling only reads as blocking when the daemon says it is", () => {
  const reached = { spend_ceiling_usd: 10, spend_window_usd: 12 };
  assert.equal(spendCeiling(reached).blocked, false, "reached is not the same as blocking");
  assert.equal(
    spendCeiling({ ...reached, admission_block: "spend-ceiling" }).blocked,
    true,
  );
  assert.equal(
    spendCeiling({ ...reached, admission_block: "slots-full" }).blocked,
    false,
    "the slot cap is reported ahead of the ceiling and must not be attributed to it",
  );
});

test("an unreported window spend counts as zero, never as negative", () => {
  const info = spendCeiling({ spend_ceiling_usd: 10, spend_window_usd: undefined });
  assert.equal(info.spent, 0);
  assert.equal(info.percent, 0);
});

test("the title always names which agents a per-session budget binds", () => {
  const title = spendCeilingTitle(
    { spend_ceiling_usd: 10, spend_window_usd: 2, spend_window_hours: 24, budget_enforced_agents: ["claude"] },
    t,
  );
  assert.match(title, /spend-enforced-agents/);
  assert.match(title, /agents=claude/);
  assert.match(title, /spend-ceiling-of/);
});

test("with no ceiling the title still states what is enforced", () => {
  const title = spendCeilingTitle({ budget_enforced_agents: ["claude"] }, t);
  assert.match(title, /spend-no-ceiling/);
  assert.match(title, /spend-enforced-agents/);
});

test("an empty enforcement list says so instead of implying a hard stop", () => {
  const title = spendCeilingTitle({ spend_ceiling_usd: 5, budget_enforced_agents: [] }, t);
  assert.match(title, /spend-enforced-none/);
});

test("a blocking ceiling says running sessions are untouched", () => {
  const title = spendCeilingTitle(
    {
      spend_ceiling_usd: 10,
      spend_window_usd: 11,
      admission_block: "spend-ceiling",
      budget_enforced_agents: ["claude"],
    },
    t,
  );
  assert.match(title, /spend-ceiling-blocking/);
});
