/**
 * Reading provider quota state off a session row.
 *
 * Split out of `main.js` so the parts that can silently produce a wrong answer
 * — unwrapping a serde `SystemTime`, and deciding what to show when the agent
 * reported no reset time — are executable in a test rather than only greppable.
 *
 * The rule these all follow: a rate limit is reported, never inferred, and an
 * absent detail is said out loud rather than replaced with a plausible number.
 * A guessed reset time sends the operator back to a session the provider is
 * still refusing, which is worse than admitting the agent did not say.
 */

import { optionalSystemTimeMs } from "./time.js";

/**
 * Milliseconds since the epoch for a limit's reset, or null when there is none.
 *
 * Rust's `SystemTime` crosses serde as `{ secs_since_epoch, nanos_since_epoch }`.
 * `Number()` on that object is `NaN`, and a bare `?? 0` would render the Unix
 * epoch as a window that reopened decades ago, so both are rejected explicitly.
 */
export function resetMillis(rateLimit) {
  const resets = rateLimit?.resets_at;
  if (resets === null || resets === undefined) return null;
  return optionalSystemTimeMs(resets);
}

/** Whole minutes until `millis`, never negative — a window past due reads "0". */
export function minutesUntil(millis, now = Date.now()) {
  return Math.max(0, Math.round((millis - now) / 60000));
}

/**
 * The row label for a limited session: which quota, and when it reopens.
 * Either detail is omitted when the agent did not report it.
 */
export function rateLimitedLabel(session, t, now = Date.now()) {
  const base = t("status-rate-limited");
  const limit = session?.rate_limit;
  if (!limit) return base;
  const parts = [base];
  if (limit.scope) parts.push(t("rate-limit-row", { scope: limit.scope }));
  const resets = resetMillis(limit);
  if (resets !== null) {
    parts.push(t("rate-limit-in-minutes", { minutes: minutesUntil(resets, now) }));
  }
  return parts.join(" · ");
}

/**
 * The fleet header's summary of every limited session: how many, and when the
 * soonest window reopens. Sessions that reported no reset time are counted but
 * cannot contribute a time, so a fleet where none reported says exactly that.
 */
export function rateLimitTitle(limited, t, now = Date.now()) {
  const resets = limited
    .map((session) => resetMillis(session.rate_limit))
    .filter((value) => value !== null);
  if (!resets.length) return t("rate-limit-reset-unknown", { count: limited.length });
  return t("rate-limit-resets-in", {
    count: limited.length,
    minutes: minutesUntil(Math.min(...resets), now),
  });
}
