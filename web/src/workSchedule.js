/**
 * What a standing schedule is about to do, and what it did last time.
 *
 * Split out of `main.js` for the reason the other extracted modules were: the
 * decisions here are small, they are all about *reporting truthfully*, and each
 * one is wrong in a way a screenshot would not show. A schedule the operator
 * was not present for is only worth having if the window can say what it did
 * while they were away — including the firings that deliberately did nothing.
 *
 * Nothing here talks to the backend. The caller passes the schedule as the
 * command returned it, and this decides what to say about it.
 */

/** The cadences the window offers. Seconds, because that is what the command takes. */
export const REPEAT_CHOICES = [
  { seconds: 4 * 3600, key: "work-repeat-hours", args: { hours: 4 } },
  { seconds: 8 * 3600, key: "work-repeat-hours", args: { hours: 8 } },
  { seconds: 12 * 3600, key: "work-repeat-hours", args: { hours: 12 } },
  { seconds: 24 * 3600, key: "work-repeat-hours", args: { hours: 24 } },
  { seconds: 7 * 24 * 3600, key: "work-repeat-days", args: { days: 7 } },
];

/**
 * Serde writes `SystemTime` as `{ secs_since_epoch, nanos_since_epoch }`.
 * Anything else is not a time, and guessing one would put a confident next-run
 * time on a schedule that has none.
 */
export function scheduleTimeMs(value) {
  const seconds = value?.secs_since_epoch;
  if (typeof seconds !== "number" || !Number.isFinite(seconds)) return null;
  return seconds * 1000 + Math.floor((value.nanos_since_epoch ?? 0) / 1e6);
}

/**
 * Round a wait down to the coarsest unit that still describes it.
 *
 * Returns the arguments for a catalog message rather than a formatted string:
 * the pluralisation belongs to Fluent, not here.
 */
export function countdown(nextDueMs, nowMs) {
  if (nextDueMs === null) return null;
  const seconds = Math.round((nextDueMs - nowMs) / 1000);
  // Due, or overdue because nothing was running to fire it. Both are "now" to
  // an operator: the next check will pick it up.
  if (seconds <= 60) return { key: "work-schedule-due", args: {} };
  if (seconds < 3600) {
    return { key: "work-schedule-next-minutes", args: { minutes: Math.round(seconds / 60) } };
  }
  if (seconds < 24 * 3600) {
    return { key: "work-schedule-next-hours", args: { hours: Math.round(seconds / 3600) } };
  }
  return { key: "work-schedule-next-days", args: { days: Math.round(seconds / (24 * 3600)) } };
}

/**
 * What the last firing did, as catalog message arguments.
 *
 * A firing that skipped is reported with its reason. A schedule that says
 * nothing about the firings that did nothing is one the operator has to check
 * by hand, which is what they set a schedule to avoid.
 */
export function lastFiringMessage(firing) {
  if (!firing) return null;
  const result = firing.result ?? {};
  if (result.kind === "started") {
    return { key: "work-schedule-last-started", args: { count: Number(result.projects ?? 0) } };
  }
  if (result.kind === "skipped") {
    return { key: "work-schedule-last-skipped", args: { reason: String(result.reason ?? "") } };
  }
  return null;
}

/**
 * Occurrences that came due with nothing running to fire them.
 *
 * Reported rather than made up for: firing them in a burst would put the same
 * prompt into the same repositories several times over, each landing on the
 * last one's uncommitted work. Silence about them would leave the operator
 * believing every occurrence ran.
 */
export function missedMessage(firing) {
  const missed = Number(firing?.missed ?? 0);
  if (!Number.isFinite(missed) || missed <= 0) return null;
  return { key: "work-schedule-missed", args: { count: missed } };
}

/**
 * The whole status line, as an ordered list of catalog messages to join.
 *
 * `null` when there is no schedule at all — the caller hides the line rather
 * than showing an empty one.
 */
export function scheduleStatus(schedule, nowMs) {
  if (!schedule) return null;
  const parts = [];
  if (schedule.paused) {
    parts.push({ key: "work-schedule-paused", args: {} });
  } else {
    const next = countdown(scheduleTimeMs(schedule.next_due), nowMs);
    if (next) parts.push(next);
  }
  const last = lastFiringMessage(schedule.history?.[0]);
  if (last) parts.push(last);
  const missed = missedMessage(schedule.history?.[0]);
  if (missed) parts.push(missed);
  return parts;
}
