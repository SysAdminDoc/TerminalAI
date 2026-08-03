/**
 * Deciding who can receive a broadcast prompt.
 *
 * Split out of `main.js` because the eligibility rule is a safety rule, not a
 * presentation detail, and it has to agree exactly with the daemon's — a UI
 * that offers a session the daemon will refuse teaches the operator to ignore
 * the refusals.
 *
 * The rule: a session receives a broadcast only if a process is behind it and
 * it is not waiting on a permission decision. A permission prompt is a specific
 * question with a small set of valid answers, and typing a paragraph of prompt
 * text at it answers something — just not what the operator meant.
 */

/** Statuses with no live process behind them. */
const NOT_RUNNING = new Set(["exited", "queued"]);

/**
 * Why this session cannot take a broadcast, or null when it can.
 *
 * Returns a message key rather than text so the caller localizes it, and so a
 * reason with no string fails a test rather than rendering as `undefined`.
 */
export function ineligibleReason(session) {
  if (!session) return "broadcast-skip-not-running";
  if (session.status === "needs-approval") return "broadcast-skip-approval";
  if (NOT_RUNNING.has(session.status)) return "broadcast-skip-not-running";
  return null;
}

export function isEligible(session) {
  return ineligibleReason(session) === null;
}

/**
 * The broadcast target list: every session, eligible ones first.
 *
 * Ineligible sessions are listed rather than hidden. Hiding them would make the
 * fleet look smaller than it is at the moment the operator is deciding who to
 * send to, and the reason a session is missing is exactly the thing worth
 * saying.
 */
export function targets(sessions = []) {
  return sessions
    .map((session) => ({
      session,
      reason: ineligibleReason(session),
    }))
    .sort((left, right) => {
      const eligibility = Number(Boolean(left.reason)) - Number(Boolean(right.reason));
      if (eligibility !== 0) return eligibility;
      return String(left.session.id).localeCompare(String(right.session.id));
    });
}

/**
 * Which sessions a freshly opened dialog should have ticked.
 *
 * Every eligible session, and only those. Pre-ticking a session that cannot
 * receive the prompt produces a refusal the operator did not ask for; leaving
 * everything unticked makes the common case — "all of them" — the most work.
 */
export function defaultSelection(sessions = []) {
  return sessions.filter(isEligible).map((session) => session.id);
}

/**
 * Summarize what came back from the daemon.
 *
 * Delivered and refused are reported separately and always: "sent" alone, when
 * four of nine were skipped, is the failure this whole per-session protocol
 * exists to prevent.
 */
export function summarize(results = []) {
  const delivered = results.filter((result) => !result.refusal);
  return {
    delivered: delivered.length,
    refused: results.length - delivered.length,
    total: results.length,
  };
}
