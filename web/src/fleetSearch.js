/**
 * Rendering fleet search results.
 *
 * Its own module because what it renders is the widest untrusted-content path
 * in the application: a search excerpt is arbitrary agent output — whatever a
 * tool printed, including the contents of a file it read. Kept importable so
 * the escaping can be asserted against real markup rather than by grepping
 * `main.js` for the characters `escapeHtml`, which passes whether or not the
 * value reaching the DOM was ever escaped.
 */

/**
 * One session's matches.
 *
 * The count shown is the session's own total, which stays exact even when the
 * excerpts below it were capped — so a truncated result says how much it did
 * not show rather than looking complete.
 */
export function renderSearchMatch(session, { escape, translate }) {
  const excerpts = session.hits
    .map(
      (hit) =>
        `<li><span class="search-hit-line">${escape(String(hit.line))}</span>` +
        `<code>${escape(hit.text)}</code></li>`,
    )
    .join("");
  const capped = session.truncated
    ? `<small class="search-truncated">${escape(translate("fleet-search-truncated"))}</small>`
    : "";
  return (
    `<section class="search-result"><div class="search-result-head">` +
    `<button type="button" class="button button-quiet" data-search-focus="${escape(session.id)}">` +
    `${escape(session.name)}</button>` +
    `<span class="search-result-count">` +
    `${escape(translate("fleet-search-hits", { count: session.total_matches }))}` +
    `</span></div><ul class="search-hits">${excerpts}</ul>${capped}</section>`
  );
}

/** The whole result set, or the empty-state message. */
export function renderSearchResults(matches, { escape, translate, needle }) {
  if (!matches.length) {
    return `<p class="rollup-total">${escape(translate("fleet-search-none", { needle }))}</p>`;
  }
  return matches
    .map((session) => renderSearchMatch(session, { escape, translate }))
    .join("");
}

/** Sessions matched, and occurrences across all of them. */
export function searchSummary(matches) {
  return {
    sessions: matches.length,
    total: matches.reduce((sum, session) => sum + session.total_matches, 0),
  };
}
