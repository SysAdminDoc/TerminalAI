/**
 * Adding up what the fleet has spent.
 *
 * Split out of `main.js` for the same reason as `rateLimit.js`: every function
 * here can produce a confidently wrong number, and a wrong number in a spend
 * report is indistinguishable from a right one.
 *
 * The rule all of these follow: **a session that has not been priced is
 * excluded and counted, never treated as zero.** Zero is a claim — that the
 * session ran and cost nothing. An unread transcript is not that claim, and
 * folding it in as zero makes the fleet total quietly too low exactly when the
 * operator is checking whether it is too high.
 */

/** Token fields, in the order a report should read them. */
export const TOKEN_FIELDS = [
  ["input_tokens", "tokens-input"],
  ["output_tokens", "tokens-output"],
  ["cache_read_input_tokens", "tokens-cache-read"],
  ["cache_creation_input_tokens", "tokens-cache-write"],
];

/** True when a session has a cost figure derived from a real transcript. */
export function isPriced(session) {
  return session?.cost_usd !== null && session?.cost_usd !== undefined;
}

function emptyTotals() {
  return {
    cost_usd: 0,
    requests: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    /** Sessions that contributed a figure. */
    priced: 0,
    /** Sessions with no transcript read yet. Reported, never folded in. */
    unpriced: 0,
  };
}

function absorb(totals, session) {
  if (!isPriced(session)) {
    totals.unpriced += 1;
    return totals;
  }
  totals.priced += 1;
  totals.cost_usd += Number(session.cost_usd) || 0;
  const tokens = session.tokens;
  if (tokens) {
    totals.requests += Number(tokens.requests) || 0;
    for (const [field] of TOKEN_FIELDS) {
      totals[field] += Number(tokens[field]) || 0;
    }
  }
  return totals;
}

/** Everything the fleet has spent, with the unpriced sessions counted apart. */
export function fleetTotals(sessions = []) {
  return sessions.reduce((totals, session) => absorb(totals, session), emptyTotals());
}

/**
 * Group sessions and total each group.
 *
 * Returned sorted by cost, descending: the question a rollup answers is "what
 * is expensive", so the answer belongs at the top rather than wherever the
 * group's name happens to sort. Ties fall back to the key so the order is
 * stable between renders rather than following map insertion.
 */
export function rollupBy(sessions = [], keyOf) {
  const groups = new Map();
  for (const session of sessions) {
    const key = keyOf(session) ?? "—";
    if (!groups.has(key)) groups.set(key, emptyTotals());
    absorb(groups.get(key), session);
  }
  return [...groups.entries()]
    .map(([key, totals]) => ({ key, ...totals }))
    .sort((left, right) => right.cost_usd - left.cost_usd || left.key.localeCompare(right.key));
}

/** The folder a session runs in, by its last path component. */
export function folderOf(session) {
  const cwd = session?.cwd ?? "";
  const parts = cwd.split(/[\\/]/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : cwd || "—";
}

/**
 * A token count for a dense row.
 *
 * Thousands separators only below a million; above it, three more digits of
 * precision tell the operator nothing they would act on, and the column has to
 * stay narrow enough to sit beside a cost.
 */
export function formatTokens(count) {
  const value = Number(count) || 0;
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 10_000) return `${Math.round(value / 1000)}k`;
  return value.toLocaleString("en-US");
}

/** A cost, or an em dash when nothing has priced it. */
export function formatCost(amount) {
  if (amount === null || amount === undefined) return "—";
  const value = Number(amount);
  if (!Number.isFinite(value)) return "—";
  // Two decimals hides the difference between a free session and a nearly free
  // one; below a cent, say so rather than rounding it away to $0.00.
  if (value > 0 && value < 0.01) return "<$0.01";
  return `$${value.toFixed(2)}`;
}

/**
 * How the rollup describes its own coverage.
 *
 * Always rendered, including when everything is priced — a total with no
 * statement of what it covers invites being read as the whole fleet.
 */
export function coverage(totals, t) {
  if (!totals.priced && !totals.unpriced) return t("rollup-empty");
  if (!totals.unpriced) return t("rollup-complete", { priced: totals.priced });
  return t("rollup-partial", { priced: totals.priced, unpriced: totals.unpriced });
}
