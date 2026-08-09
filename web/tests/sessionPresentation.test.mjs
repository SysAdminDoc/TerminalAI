import assert from "node:assert/strict";
import test from "node:test";

import { createSessionPresentation } from "../src/sessionPresentation.js";

const t = (key, args) => (args ? `${key}:${JSON.stringify(args)}` : key);
const presentation = createSessionPresentation({ relativeDwell: (value) => `dwell:${value}`, t });

test("session presentation keeps progress, ports, folders, and memory honest", () => {
  assert.equal(presentation.toolProgress({ completed: 3, total: 5 }), "3/5");
  assert.equal(presentation.toolProgress({ completed: 3, total: 0 }), "—");
  assert.equal(presentation.ports([4100, 4101, 4102]), "4100–4102");
  assert.equal(presentation.ports([4100, 4102]), "4100, 4102");
  assert.equal(presentation.ports([0, 70000]), "—");
  assert.equal(presentation.folderLabel("C:\\Users\\me\\repo"), "repo");
  assert.equal(presentation.memory(0), "—");
  assert.equal(presentation.memory(1024 * 1024 * 5), "5 MB");
  assert.equal(presentation.memory(1024 * 1024 * 1024 * 2.5), "2.5 GB");
});

test("cost and memory titles distinguish missing budgets and scoped readings", () => {
  assert.equal(presentation.cost(null), "—");
  assert.equal(presentation.cost(""), "—");
  assert.equal(presentation.costTitle({}), "cost-explained");
  assert.match(presentation.costTitle({ budget_usd: 5 }), /^cost-budget-of:/);
  assert.equal(presentation.memoryTitle({ memory_limited: true }), "memory-limited-explained");
  assert.equal(presentation.memoryTitle({}), "memory-unscoped-explained");
  assert.equal(presentation.memoryTitle({ memory_processes: 3 }), 'memory-explained:{"processes":3}');
});

test("only self-resolving questions receive a bounded answer deadline", () => {
  const now = 1_700_000_000_000;
  const statusSince = {
    secs_since_epoch: Math.floor((now - 12_000) / 1000),
    nanos_since_epoch: 0,
  };
  assert.equal(presentation.isAttention({ status: "needs-you" }), true);
  assert.equal(presentation.isAttention({ status: "working" }), false);
  assert.equal(presentation.answerSecondsRemaining({ status: "needs-approval", status_since: statusSince }, now), null);
  assert.equal(presentation.answerSecondsRemaining({ status: "needs-you", status_since: statusSince }, now), 48);
  assert.equal(presentation.answerCountdownLabel({ status: "needs-you", status_since: statusSince }, now), 'answer-deadline:{"seconds":48}');
  assert.equal(presentation.answerCountdownLabel({ status: "working" }, now), "");
});
