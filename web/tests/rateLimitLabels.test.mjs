import assert from "node:assert/strict";
import test from "node:test";

import {
  minutesUntil,
  quotaLabel,
  rateLimitTitle,
  rateLimitedLabel,
  resetMillis,
  worstQuota,
} from "../src/rateLimit.js";

/** The exact shape serde produces for `Option<SystemTime>`. */
function systemTime(epochSeconds) {
  return { secs_since_epoch: epochSeconds, nanos_since_epoch: 0 };
}

/** A stand-in for `t()` that shows which key and arguments were used. */
const t = (id, args = {}) => {
  const rendered = Object.entries(args)
    .map(([key, value]) => `${key}=${value}`)
    .join(",");
  return rendered ? `${id}(${rendered})` : id;
};

const NOW = Date.UTC(2026, 7, 3, 12, 0, 0);

test("a serde SystemTime is unwrapped, not coerced", () => {
  // Number({secs_since_epoch: …}) is NaN. Coercing would drop every reset time
  // the daemon actually reported.
  assert.equal(resetMillis({ resets_at: systemTime(1785758400) }), 1785758400000);
  // A bare number is accepted too, so a future protocol change does not silently
  // start returning null.
  assert.equal(resetMillis({ resets_at: 1785758400 }), 1785758400000);
});

test("a missing reset time is null, never the epoch", () => {
  // `?? 0` here would render "reopens in -29000000 min".
  assert.equal(resetMillis({ resets_at: null }), null);
  assert.equal(resetMillis({}), null);
  assert.equal(resetMillis(null), null);
  assert.equal(resetMillis(undefined), null);
  assert.equal(resetMillis({ resets_at: "soon" }), null);
  assert.equal(resetMillis({ resets_at: {} }), null);
});

test("a window already past due reads as zero, not negative", () => {
  assert.equal(minutesUntil(NOW - 90 * 60_000, NOW), 0);
  assert.equal(minutesUntil(NOW + 45 * 60_000, NOW), 45);
  assert.equal(minutesUntil(NOW + 29_000, NOW), 0);
  assert.equal(minutesUntil(NOW + 31_000, NOW), 1);
});

test("the row names the quota that tripped and when it reopens", () => {
  const session = {
    status: "rate-limited",
    rate_limit: {
      scope: "weekly",
      resets_at: systemTime(Math.floor(NOW / 1000) + 45 * 60),
    },
  };
  assert.equal(
    rateLimitedLabel(session, t, NOW),
    "status-rate-limited · rate-limit-row(scope=weekly) · rate-limit-in-minutes(minutes=45)",
  );
});

test("a limit with no reset time still says the session is blocked", () => {
  // Claude's retry events often carry a category and no delay. The operator
  // still needs to know the session is going nowhere.
  const session = { status: "rate-limited", rate_limit: { scope: "overloaded" } };
  assert.equal(
    rateLimitedLabel(session, t, NOW),
    "status-rate-limited · rate-limit-row(scope=overloaded)",
  );
});

test("a limited row with no detail at all degrades to the plain status", () => {
  assert.equal(rateLimitedLabel({ status: "rate-limited" }, t, NOW), "status-rate-limited");
});

test("the header reports the soonest window across the fleet", () => {
  const at = (minutes) => ({
    rate_limit: { scope: "primary", resets_at: systemTime(Math.floor(NOW / 1000) + minutes * 60) },
  });
  assert.equal(
    rateLimitTitle([at(120), at(15), at(60)], t, NOW),
    "rate-limit-resets-in(count=3,minutes=15)",
  );
});

test("sessions without a reset time are counted but cannot invent one", () => {
  const withTime = {
    rate_limit: { scope: "primary", resets_at: systemTime(Math.floor(NOW / 1000) + 30 * 60) },
  };
  const withoutTime = { rate_limit: { scope: "overloaded" } };
  // Counted in the total…
  assert.equal(
    rateLimitTitle([withTime, withoutTime], t, NOW),
    "rate-limit-resets-in(count=2,minutes=30)",
  );
  // …and when none of them reported, the header says so rather than showing 0.
  assert.equal(
    rateLimitTitle([withoutTime, withoutTime], t, NOW),
    "rate-limit-reset-unknown(count=2)",
  );
});

test("the header reports quota headroom before the provider refuses, not only after", () => {
  const t = (key, args) => `${key}:${JSON.stringify(args ?? {})}`;
  const now = Date.UTC(2026, 7, 4, 12, 0, 0);

  // A routine report with room left. This used to be discarded outright, so the
  // fleet could only speak about quota once work had already stopped.
  const reported = {
    id: "s1",
    status: "thinking",
    quota: { scope: "primary", used_percent: 63.5, resets_at: { secs_since_epoch: now / 1000 + 1800, nanos_since_epoch: 0 } },
  };
  const label = quotaLabel([reported], t, now);
  assert.equal(label.percent, 64);
  assert.match(label.title, /quota-used/);
  assert.match(label.title, /rate-limit-in-minutes/);

  // The tightest window across the fleet wins, and an active refusal outranks
  // the same session's older headroom reading.
  const worst = worstQuota([
    reported,
    { id: "s2", status: "rate-limited", quota: { scope: "primary", used_percent: 10 }, rate_limit: { scope: "weekly", used_percent: 100 } },
  ]);
  assert.equal(worst.used, 100);
  assert.equal(worst.session.id, "s2");
});

test("a session that reported no quota is not reported as having all of it", () => {
  const t = (key, args) => `${key}:${JSON.stringify(args ?? {})}`;
  // No quota at all, and a quota object without a percentage. Neither is a zero:
  // rendering 0% would claim the whole window is still available.
  assert.equal(quotaLabel([{ id: "s1", status: "idle" }], t), null);
  assert.equal(quotaLabel([{ id: "s1", quota: { scope: "primary" } }], t), null);
  assert.equal(worstQuota([]), null);
  assert.equal(worstQuota(undefined), null);
});
