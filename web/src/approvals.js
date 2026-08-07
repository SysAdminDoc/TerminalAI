/**
 * The approvals inbox: every session waiting on a permission decision.
 *
 * Permission prompts are the fleet's blocking work, and answering them one
 * focused session at a time is the cost this exists to remove. It is a view
 * over the snapshot the window already has — a blocked session carries its own
 * pending request — so there is no separate poll and nothing can disagree with
 * the rows behind it.
 *
 * # It never answers on the operator's behalf
 *
 * No auto-approve, no bypass mode, no "approve all". The inbox surfaces the
 * question and routes the answer; deciding is the operator's, every time. A
 * competitor was criticised for exactly the shortcut this refuses to take.
 */

/** Statuses that mean the session is waiting on a person. */
const BLOCKED = new Set(["needs-approval", "needs-you", "awaiting-input"]);

/**
 * Sessions waiting on a decision, longest wait first.
 *
 * Oldest-first is the whole ordering argument: an inbox sorted newest-first
 * puts the session that has been blocked for twenty minutes below the one that
 * asked a second ago, which is how a fleet ends up with one session starved
 * behind a stream of new prompts. A session with no recorded time sorts last
 * rather than first — an unknown wait is not evidence of a long one.
 */
export function pendingApprovals(sessions) {
  return sessions
    .filter((session) => BLOCKED.has(session.status))
    .slice()
    .sort((left, right) => {
      const a = waitingSince(left);
      const b = waitingSince(right);
      if (a === null) return b === null ? 0 : 1;
      if (b === null) return -1;
      return a - b;
    });
}

/** Milliseconds since the epoch for when this session started waiting. */
export function waitingSince(session) {
  const stamp = session?.pending_approval?.since ?? session?.status_since;
  if (stamp === null || stamp === undefined) return null;
  // The wire carries SystemTime; the existing helper is what reads it.
  const value = Number(stamp?.secs_since_epoch ?? stamp);
  return Number.isFinite(value) ? value : null;
}

/**
 * What the session is asking, as one line.
 *
 * A request with neither a tool nor a summary is not dropped: the session is
 * still blocked, and an inbox that hid the rows it could not describe would be
 * hiding exactly the ones that need a human to go and look.
 */
export function requestLine(session, translate) {
  const pending = session?.pending_approval;
  if (!pending) return translate("approvals-unknown-request");
  const tool = pending.tool?.trim();
  const summary = pending.summary?.trim();
  if (tool && summary) return `${tool} — ${summary}`;
  if (tool) return tool;
  if (summary) return summary;
  return translate("approvals-unknown-request");
}

/** One row of the inbox. */
export function renderApproval(session, { escape, translate, dwell }) {
  const detail = requestLine(session, translate);
  return (
    `<section class="approval" data-approval="${escape(session.id)}">` +
    `<div class="search-result-head">` +
    `<button type="button" class="button button-quiet" data-approval-focus="${escape(session.id)}">` +
    `${escape(session.name)}</button>` +
    `<span class="search-result-count">${escape(dwell(session))}</span></div>` +
    `<code class="approval-request">${escape(detail)}</code>` +
    // The answer box, not an approve button. What the agent accepts is its own
    // prompt's vocabulary — a number, a letter, a word — and inventing a
    // universal "yes" for it would be answering on the operator's behalf with
    // a keystroke nobody checked.
    `<div class="approval-answer">` +
    `<input type="text" maxlength="500" data-approval-reply="${escape(session.id)}" ` +
    `placeholder="${escape(translate("approvals-answer-placeholder"))}" ` +
    `aria-label="${escape(translate("approvals-answer-for", { name: session.name }))}" />` +
    `<button type="button" class="button button-primary" data-approval-send="${escape(session.id)}">` +
    `${escape(translate("approvals-send"))}</button></div></section>`
  );
}

export function renderApprovals(sessions, deps) {
  if (!sessions.length) {
    return `<p class="rollup-total">${deps.escape(deps.translate("approvals-empty"))}</p>`;
  }
  return sessions.map((session) => renderApproval(session, deps)).join("");
}
