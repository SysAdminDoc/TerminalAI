import {
  coverage,
  fleetTotals,
  folderOf,
  formatCost,
  formatTokens,
  rollupBy,
  TOKEN_FIELDS,
} from "./rollup.js";
import { renderWindowShares } from "./quotaWindow.js";

/**
 * Spend rollup page orchestration.
 *
 * The arithmetic remains in rollup.js; this boundary owns grouping the live
 * fleet into the page's tables and opening the guarded dialog.
 */
export function createRollupPage(deps) {
  const { $, escapeHtml, renderGuarded, state, t } = deps;

  /**
   * Break the fleet's spend down by agent, by folder, and by session.
   *
   * One aggregate answers "are we spending too much" and nothing else. These
   * three groupings answer the question that follows it — which is always "on
   * what" — and every one of them states how many sessions it could not price,
   * because a total that quietly omits half the fleet is worse than no total.
   */
  function renderRollup() {
    const sessions = state.sessions;
    const totals = fleetTotals(sessions);
    $("rollup-coverage").textContent = coverage(totals, t);

    if (!sessions.length) {
      $("rollup-body").innerHTML = `<p class="rollup-total surface-empty">${escapeHtml(t("rollup-empty"))}</p>`;
      return;
    }

    const tokenCells = (row) =>
      TOKEN_FIELDS.map(([field]) =>
        "<td class=\"rollup-number\">" +
        escapeHtml(formatTokens(row[field])) +
        "</td>",
      ).join("");
    const groupTable = (titleKey, rows, label = (row) => row.key) => {
      const tokenHeaders = TOKEN_FIELDS.map(
        ([, key]) =>
          "<th class=\"rollup-number\">" + escapeHtml(t(key)) + "</th>",
      ).join("");
      const body = rows
        .map((row) => {
          const rowLabel = escapeHtml(label(row));
          const unpriced = row.unpriced
            ? '<small class="rollup-unpriced"> +' + row.unpriced + "</small>"
            : "";
          const cost = formatCost(row.priced ? row.cost_usd : null);
          return "<tr><th scope=\"row\">" +
            rowLabel +
            unpriced +
            "</th><td class=\"rollup-number\">" +
            escapeHtml(cost) +
            "</td>" +
            tokenCells(row) +
            "</tr>";
        })
        .join("");
      return [
        '<section class="rollup-section">',
        "<h3>" + escapeHtml(t(titleKey)) + "</h3>",
        '<table class="rollup-table">',
        "<thead><tr>",
        "<th>" + escapeHtml(t(titleKey)) + "</th>",
        '<th class="rollup-number">$</th>',
        tokenHeaders,
        "</tr></thead>",
        "<tbody>" + body + "</tbody>",
        "</table>",
        "</section>",
      ].join("");
    };

    // Sessions are their own grouping so a single expensive run is visible
    // rather than hidden inside its folder's subtotal.
    const bySession = rollupBy(sessions, (session) => session.id).map((row) => {
      const session = sessions.find((item) => item.id === row.key);
      return { ...row, label: session ? String(session.id) + " · " + String(session.name) : row.key };
    });

    $("rollup-body").innerHTML = [
      groupTable("rollup-by-agent", rollupBy(sessions, (session) => session.agent)),
      groupTable("rollup-by-folder", rollupBy(sessions, folderOf)),
      groupTable("rollup-by-session", bySession, (row) => row.label),
      // The window breakdown sits with the rollup because it is the same
      // arithmetic asked a different question: not what a session has cost,
      // but what it spent inside the window a provider is currently refusing.
      renderWindowShares(state.admission, sessions, {
        escape: escapeHtml,
        translate: t,
        cost: (usd) => formatCost(usd),
        hours: Number(state.admission?.spend_window_hours) || 24,
      }),
      '<section class="rollup-section rollup-total"><h3>' +
        escapeHtml(t("rollup-total")) +
        "</h3><p><b>" +
        escapeHtml(formatCost(totals.priced ? totals.cost_usd : null)) +
        "</b> · " +
        escapeHtml(String(totals.requests)) +
        " " +
        escapeHtml(t("rollup-requests")) +
        "</p></section>",
    ].join("");
  }

  function openRollup() {
    const dialog = $("rollup-dialog");
    if (!dialog.open) dialog.showModal();
    renderGuarded(
      $("rollup-body"),
      t("rollup-render-error"),
      "openRollup",
      openRollup,
      renderRollup,
    );
  }

  return { openRollup, renderRollup };
}
