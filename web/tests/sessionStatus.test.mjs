import assert from "node:assert/strict";
import test from "node:test";

import { rateLimitedLabel } from "../src/rateLimit.js";
import {
  createSessionStatus,
  SILENCE_THRESHOLD_MINUTES,
  STALL_THRESHOLD_MINUTES,
  STATUS_KEYS,
  STATUS_META,
  STATUS_ORDER,
} from "../src/sessionStatus.js";

const t = (key, args) => (args ? `${key}:${JSON.stringify(args)}` : key);
const status = createSessionStatus({ t, rateLimitedLabel });

test("status metadata exposes every ordered row state", () => {
  assert.deepEqual(STATUS_KEYS, Object.keys(STATUS_META));
  assert.ok(STATUS_ORDER["needs-approval"] > STATUS_ORDER.working);
  assert.ok(STATUS_ORDER["rate-limited"] > STATUS_ORDER.thinking);
  assert.equal(STATUS_META["needs-you"].tone, "peach");
  assert.notEqual(STATUS_META["needs-approval"].glyph, STATUS_META["needs-you"].glyph);
});

test("last activity prefers transcript text and has an honest fallback", () => {
  assert.equal(status.lastActivity({ last_message: "agent said this", last_line: "raw" }), "agent said this");
  assert.equal(status.lastActivity({ last_message: "  ", last_line: "raw" }), "raw");
  assert.equal(status.lastActivity({ last_line: "" }), "empty-no-output");
});

test("lifecycle labels and details preserve failure evidence", () => {
  assert.equal(status.lifecycleLabel({ phase: "preparing" }), "status-preparing");
  assert.equal(
    status.lifecycleLabel({ phase: "failed", restarts: 3 }),
    'status-failed:{"restarts":3}',
  );
  assert.equal(status.lifecycleDetail({ phase: "failed", restarts: 2, last_exit_code: 17 }), 'status-failed-detail-code:{"restarts":2,"code":17}');
  assert.equal(status.lifecycleDetail({ phase: "finished" }), "status-finished-detail");
  assert.equal(status.lifecycleTone({ phase: "failed" }, STATUS_META.exited), "red");
  assert.equal(status.lifecycleTone({ phase: "finished" }, STATUS_META.exited), "green");
});

test("stalled and silent sessions are louder than healthy work", () => {
  assert.equal(STALL_THRESHOLD_MINUTES, 15);
  assert.equal(SILENCE_THRESHOLD_MINUTES, 15);
  const stalled = { status: "working", stalled: true };
  const silent = { status: "working", health: "unresponsive" };
  assert.equal(status.lifecycleLabel(stalled), 'status-stalled:{"status":"status-working"}');
  assert.equal(status.lifecycleDetail(silent), 'status-unresponsive-detail:{"minutes":15}');
  assert.equal(status.lifecycleTone(stalled, STATUS_META.working), "peach");
  assert.equal(status.lifecycleTone(silent, STATUS_META.working), "peach");
});

test("unknown status labels do not invent a translation key", () => {
  assert.equal(status.statusLabel("provider-specific"), "provider-specific");
  assert.equal(status.statusLabel(undefined), "status-unknown");
  assert.equal(status.metaLabel(STATUS_META.working), "status-working");
});
