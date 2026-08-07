/**
 * Who consumed the quota window that is refusing the fleet right now.
 *
 * The header already says a provider is rate limiting and when the window
 * reopens. It could not say which sessions spent it — and subscription-window
 * exhaustion is the loudest operational complaint about the agents this tool
 * supervises, where "which one is eating it" is the whole question.
 *
 * A session's running cost cannot answer that: it includes everything spent
 * before the window opened. These figures come from the daemon's spend ledger,
 * which buckets by minute and can therefore be restricted to a window.
 *
 * Two rules this module exists to keep:
 *
 *   - **It is never presented as the provider's accounting.** These are this
 *     tool's own transcript arithmetic against a vendored price table. The
 *     wording follows the price-table tooltip already used elsewhere.
 *   - **Spend with no owner is shown, not dropped.** A ledger restored from a
 *     store written before it had a session dimension has money and no
 *     sessions. A breakdown that quietly omitted it would read as a complete
 *     account of the window and would not be one.
 */

/** Rows worth rendering, largest first, with their share of the window. */
export function windowShares(admission, sessions = []) {
  const entries = Array.isArray(admission?.spend_window_by_session)
    ? admission.spend_window_by_session
    : [];
  const total = Number(admission?.spend_window_usd);
  const denominator = Number.isFinite(total) && total > 0 ? total : null;
  return entries
    .filter((entry) => Number.isFinite(Number(entry?.usd)) && Number(entry.usd) > 0)
    .map((entry) => {
      const session = sessions.find((item) => item?.id === entry.id);
      const usd = Number(entry.usd);
      return {
        id: entry.id,
        // A session that has since exited still consumed the window, so it is
        // named by id rather than dropped for having no row.
        name: session?.name ?? null,
        usd,
        // `null`, not zero, when there is no denominator: a share of an unknown
        // total is not a share of nothing.
        percent: denominator === null ? null : (usd / denominator) * 100,
      };
    });
}

/** Window spend the ledger could not attribute, or `0`. */
export function unattributed(admission) {
  const value = Number(admission?.spend_window_unattributed_usd);
  return Number.isFinite(value) && value > 0 ? value : 0;
}

/**
 * The section's markup, or an empty string when there is nothing to say.
 *
 * Deliberately empty rather than an empty table: a window nobody has spent
 * anything in is not a table with no rows, it is a question that does not
 * arise yet.
 */
export function renderWindowShares(admission, sessions, { escape, translate, cost, hours }) {
  const shares = windowShares(admission, sessions);
  const orphaned = unattributed(admission);
  if (!shares.length && !orphaned) return "";
  const rows = shares
    .map((share) => {
      const label = share.name ? `${share.id} · ${share.name}` : share.id;
      const percent =
        share.percent === null
          ? ""
          : `<td class="rollup-number">${escape(`${Math.round(share.percent)}%`)}</td>`;
      return (
        `<tr><th scope="row">${escape(label)}</th>` +
        `<td class="rollup-number">${escape(cost(share.usd))}</td>${percent}</tr>`
      );
    })
    .join("");
  const remainder = orphaned
    ? `<tr class="quota-window-unattributed"><th scope="row">` +
      `${escape(translate("quota-window-unattributed"))}</th>` +
      `<td class="rollup-number">${escape(cost(orphaned))}</td><td></td></tr>`
    : "";
  return (
    `<section class="rollup-section"><h3>` +
    `${escape(translate("quota-window-title", { hours: Math.round(hours) }))}</h3>` +
    `<p class="rollup-unpriced-note">${escape(translate("quota-window-estimate"))}</p>` +
    `<table class="rollup-table"><thead><tr>` +
    `<th>${escape(translate("quota-window-session"))}</th>` +
    `<th class="rollup-number">$</th>` +
    `<th class="rollup-number">${escape(translate("quota-window-share"))}</th>` +
    `</tr></thead><tbody>${rows}${remainder}</tbody></table></section>`
  );
}
