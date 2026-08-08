import { countMessage } from "./i18n.js";
import { quotaLabel, quotaUnreportedLabel, rateLimitTitle } from "./rateLimit.js";
import { STATUS_KEYS, STATUS_META } from "./sessionStatus.js";
import { PRICING_STALE_AFTER_DAYS, pricingFreshness } from "./rollup.js";
import { spendCeiling, spendCeilingTitle } from "./spendCeiling.js";

/**
 * Fleet header rendering.
 *
 * The summary is a live view of admission, attention, spend, quota and status
 * coverage. Keeping it behind one boundary makes its DOM-write optimization
 * and its accounting caveats reviewable without the row lifecycle.
 */
export function createFleetSummary(deps) {
  const { $, escapeHtml, metaLabel, state, t } = deps;

  function renderSummary() {
    // Matches the daemon's admission count, which excludes rate-limited sessions:
    // they hold no slot, so counting them as live would contradict the queue.
    const live = state.sessions.filter(
      (session) => !["exited", "queued", "rate-limited"].includes(session.status),
    ).length;
    const limited = state.sessions.filter((session) => session.status === "rate-limited");
    const queued = state.sessions.filter((session) => session.status === "queued").length;
    const needsAttention = state.sessions.filter((session) =>
      ["needs-you", "needs-approval", "awaiting-input"].includes(session.status),
    ).length;
    const working = state.sessions.filter((session) => ["working", "thinking"].includes(session.status)).length;
    const reporting = state.sessions.filter(
      (session) => session.cost_usd !== null && session.cost_usd !== undefined,
    );
    const spend = reporting.reduce((total, session) => total + (Number(session.cost_usd) || 0), 0);
    const spendLabel = reporting.length ? `$${spend.toFixed(2)}` : "—";
    // A price table has a date; a figure priced against an unnamed table cannot
    // be checked. Say which one produced this number.
    const pricingVersion = state.admission.pricing_version || "no price table";
    // A price table has a date, and until now nothing aged them: a table months
    // out of date reported spend with exactly the same confidence as a current one.
    const freshness = pricingFreshness(state.admission, Date.now());
    const ageNote =
      freshness.state === "undated"
        ? t("pricing-age-undated")
        : freshness.state === "stale"
          ? t("pricing-age-stale", { days: freshness.days, threshold: PRICING_STALE_AFTER_DAYS })
          : t("pricing-age-current", { days: freshness.days });
    const reportingNote = reporting.length
      ? t("pricing-reporting", {
          pricing: pricingVersion,
          reporting: reporting.length,
          sessions: state.sessions.length,
        })
      : t("pricing-none", { pricing: pricingVersion });
    const spendTitle = `${reportingNote}. ${ageNote}`;
    const maxLive = state.admission.max_live_sessions ?? 3;
    // The ceiling is fleet-wide and refuses admission; it never stops a running
    // session, so it belongs beside the spend figure rather than in the row list.
    const ceiling = spendCeiling(state.admission);
    const ceilingTitle = spendCeilingTitle(state.admission, t);
    const limitedSummary = limited.length
      ? '<span class="summary-separator">/</span><span class="summary-item summary-limited" title="' +
        escapeHtml(rateLimitTitle(limited, t)) +
        '">' +
        escapeHtml(countMessage("count-rate-limited", limited.length)) +
        "</span>"
      : "";
    // Headroom, not refusal. The agents report this continuously and the fleet
    // used to drop it at this boundary, so it could only say "rate limited" after
    // work had already stopped - with the number that would have warned first
    // sitting on the session all along.
    const quota = quotaLabel(state.sessions, t);
    const quotaSummary =
      '<span class="summary-separator">/</span><span class="summary-item' +
      (quota && quota.percent >= 80 ? " summary-limited" : "") +
      '" title="' +
      escapeHtml(quota ? quota.title : quotaUnreportedLabel(t)) +
      '">' +
      escapeHtml(quota ? t("fleet-quota", { percent: quota.percent }) : t("fleet-quota-unreported")) +
      "</span>";
    const summaryMarkup =
      '<span class="summary-item"><b>' +
      live +
      "/" +
      maxLive +
      "</b> " +
      escapeHtml(t("fleet-live")) +
      "</span>" +
      '<span class="summary-separator">/</span><span class="summary-item">' +
      escapeHtml(countMessage("count-queued", queued)) +
      "</span>" +
      '<span class="summary-separator">/</span><span class="summary-item summary-attention">' +
      escapeHtml(countMessage("count-needs-attention", needsAttention)) +
      "</span>" +
      '<span class="summary-separator">/</span><span class="summary-item">' +
      escapeHtml(countMessage("count-active", working)) +
      "</span>" +
      limitedSummary +
      quotaSummary +
      '<span class="summary-separator">/</span><button type="button" class="summary-item summary-spend' +
      (ceiling && ceiling.blocked ? " summary-limited" : "") +
      '" id="fleet-spend" title="' +
      escapeHtml(spendTitle + " " + ceilingTitle) +
      '" aria-label="' +
      escapeHtml(t("button-open-rollup")) +
      '"><b>' +
      spendLabel +
      "</b> " +
      escapeHtml(t("fleet-spent")) +
      (ceiling ? " " + escapeHtml("(" + ceiling.percent + "% of cap)") : "") +
      "</button>";
    const summary = $("fleet-summary");
    if (summary.innerHTML !== summaryMarkup) summary.innerHTML = summaryMarkup;
    const droppedEvents = Number(state.admission.dropped_events) || 0;
    const fleetCountText = droppedEvents
      ? countMessage("count-session", state.sessions.length) + " · " + t("event-drops", { count: droppedEvents })
      : t("tracked-sessions", { count: state.sessions.length });
    const fleetCount = $("fleet-count");
    if (fleetCount.textContent !== fleetCountText) fleetCount.textContent = fleetCountText;
    const counts = Object.fromEntries(STATUS_KEYS.map((status) => [status, 0]));
    for (const session of state.sessions) {
      if (session.status in counts) counts[session.status] += 1;
    }
    const stateMarkup = STATUS_KEYS.map((status) => {
      const meta = STATUS_META[status];
      // `data-status` names the key, not the label. The browser audit reads it
      // to discover every status the fleet models and builds its fixture from.
      return '<span class="state-chip tone-' +
        escapeHtml(meta.tone) +
        '" data-status="' +
        escapeHtml(status) +
        '" role="listitem" title="' +
        escapeHtml(metaLabel(meta)) +
        ": " +
        escapeHtml(counts[status]) +
        '" aria-label="' +
        escapeHtml(metaLabel(meta)) +
        ": " +
        escapeHtml(counts[status]) +
        '"><span class="state-chip-glyph" aria-hidden="true">' +
        meta.glyph +
        "</span><b>" +
        counts[status] +
        "</b><span>" +
        escapeHtml(t(meta.short)) +
        "</span></span>";
    }).join("");
    const stateStrip = $("fleet-state-strip");
    if (stateStrip.innerHTML !== stateMarkup) stateStrip.innerHTML = stateMarkup;
  }

  return { renderSummary };
}
