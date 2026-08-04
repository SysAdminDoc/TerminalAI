/**
 * Reading the fleet spend ceiling off the admission snapshot.
 *
 * Split out of `main.js` because two things here are easy to get quietly wrong
 * and worth executing in a test rather than reading: a ceiling of `null` means
 * "nothing is enforced", which must not render as a ceiling of zero; and the
 * enforcement claim is per agent, because only Claude takes a per-session
 * budget flag. Saying "budget enforced" without naming the agent would promise
 * a hard stop that half the fleet does not have.
 */

/** Whole-dollar-ish formatting that keeps small figures legible. */
function money(value) {
  const amount = Number(value);
  if (!Number.isFinite(amount)) return null;
  if (amount > 0 && amount < 0.01) return "<$0.01";
  return `$${amount.toFixed(2)}`;
}

/**
 * The window's spend, its ceiling, and whether the ceiling is what is currently
 * stopping new sessions. Returns `null` when no ceiling is configured, so the
 * caller renders nothing rather than an empty limit.
 */
export function spendCeiling(admission) {
  const ceiling = admission?.spend_ceiling_usd;
  if (ceiling === null || ceiling === undefined) return null;
  const limit = Number(ceiling);
  if (!Number.isFinite(limit) || limit < 0) return null;
  const spent = Number(admission?.spend_window_usd);
  const used = Number.isFinite(spent) && spent > 0 ? spent : 0;
  const hours = Number(admission?.spend_window_hours);
  return {
    spent: used,
    ceiling: limit,
    hours: Number.isFinite(hours) && hours > 0 ? hours : null,
    // The daemon decides this; the ceiling can be reached without being the
    // reason nothing is starting, because the slot cap is reported first.
    blocked: admission?.admission_block === "spend-ceiling",
    percent: limit > 0 ? Math.min(999, Math.round((used / limit) * 100)) : 0,
  };
}

/**
 * The tooltip for the spend control: what the ceiling is, how much of it is
 * used, and — always — which agents a per-session budget actually binds.
 */
export function spendCeilingTitle(admission, t) {
  const info = spendCeiling(admission);
  const enforced = Array.isArray(admission?.budget_enforced_agents)
    ? admission.budget_enforced_agents
    : [];
  const enforcement = enforced.length
    ? t("spend-enforced-agents", { agents: enforced.join(", ") })
    : t("spend-enforced-none");
  if (!info) return t("spend-no-ceiling", { enforcement });
  const window = info.hours ? t("spend-window-hours", { hours: Math.round(info.hours) }) : "";
  const base = t("spend-ceiling-of", {
    spent: money(info.spent) ?? "$0.00",
    ceiling: money(info.ceiling) ?? "$0.00",
    window,
  });
  return info.blocked ? `${base} ${t("spend-ceiling-blocking")} ${enforcement}` : `${base} ${enforcement}`;
}
