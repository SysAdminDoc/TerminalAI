/**
 * Named fleet layouts: listing them, and reading what a restore did.
 *
 * Pure and importable, like `fleetSearch.js` and for the same reason — what it
 * renders is what the operator reads to decide whether a restore worked.
 *
 * Capturing a layout is deliberately NOT here. A `Session` does not carry the
 * launch spec that produced it, so the window has never had one; the daemon
 * does, and `save_working_set` reads them there. A capture assembled in the
 * renderer would be a layout describing something that is not running.
 */

/**
 * How a restore went, in the three categories that need different reactions.
 *
 * `queued` is deliberately not a failure: the admission gate holding a session
 * back is the gate working, and calling it one would train the operator to
 * raise the limit.
 */
export function summarizeRestore(outcomes) {
  return {
    started: outcomes.filter((outcome) => outcome.id && !outcome.queued).length,
    queued: outcomes.filter((outcome) => outcome.queued).length,
    refused: outcomes.filter((outcome) => outcome.refused).length,
  };
}

/** The result list, with each refusal quoted rather than summarised away. */
export function renderRestoreOutcomes(outcomes, { escape, translate }) {
  return outcomes
    .map((outcome) => {
      const tone = outcome.refused ? "restore-refused" : "restore-started";
      const detail = outcome.refused
        ? escape(outcome.refused)
        : escape(
            outcome.queued
              ? translate("working-sets-queued")
              : translate("working-sets-started"),
          );
      // A pin the fleet declined is reported next to the session it belongs to
      // rather than as a separate failure: the session did start, and the
      // operator needs to know only that it came back without its grid.
      const pin = outcome.pin_refused
        ? `<small class="restore-pin">${escape(outcome.pin_refused)}</small>`
        : "";
      return (
        `<li class="${escape(tone)}"><span class="restore-name">${escape(outcome.name)}</span>` +
        `<span class="restore-detail">${detail}</span>${pin}</li>`
      );
    })
    .join("");
}

/** One saved layout in the list. */
export function renderWorkingSet(set, { escape, translate }) {
  const pinned = set.members.filter((member) => member.pinned).length;
  return (
    `<section class="working-set"><div class="search-result-head">` +
    `<b>${escape(set.name)}</b>` +
    `<span class="search-result-count">` +
    `${escape(translate("working-sets-members", { count: set.members.length, pinned }))}` +
    `</span></div><div class="working-set-actions">` +
    `<button type="button" class="button button-primary" data-restore-set="${escape(set.name)}">` +
    `${escape(translate("working-sets-restore"))}</button>` +
    `<button type="button" class="button button-quiet" data-delete-set="${escape(set.name)}">` +
    `${escape(translate("working-sets-delete"))}</button>` +
    `</div><ul class="restore-outcomes" data-outcomes-for="${escape(set.name)}"></ul></section>`
  );
}
