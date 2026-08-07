/**
 * How full a session's context window is, as the row says it.
 *
 * Its own module, and pure, because every value here is a claim about a number
 * the operator cannot check from the screen: a percentage with no denominator,
 * or a denominator we guessed, is worse than saying nothing. The rule this
 * enforces on the display side is the same one `crates/terminalai-core/src/
 * context.rs` enforces on the data side — a window is reported, never inferred.
 */

/** Matches `ContextPressure` in the core. Kept as strings, as the wire sends them. */
export const CONTEXT_TONES = {
  comfortable: "context-comfortable",
  filling: "context-filling",
  critical: "context-critical",
};

/** Thresholds, mirroring `PRESSURE_WARN` / `PRESSURE_CRITICAL`. */
export const CONTEXT_WARN = 0.75;
export const CONTEXT_CRITICAL = 0.9;

function readingOf(session) {
  const context = session?.context;
  if (!context) return null;
  const used = Number(context.used_tokens);
  if (!Number.isFinite(used) || used < 0) return null;
  const rawWindow = Number(context.window_tokens);
  const window = Number.isFinite(rawWindow) && rawWindow > 0 ? rawWindow : null;
  return { used, window, source: context.source };
}

/** Compact token count: 41000 -> "41k", 1200000 -> "1.2M". */
export function tokenCount(value) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}k`;
  return String(value);
}

/**
 * The cell's text. An em dash when nothing has been measured, the raw
 * occupancy when no window was reported, and a percentage only when there is a
 * real denominator behind it.
 */
export function contextLabel(session) {
  const reading = readingOf(session);
  if (!reading) return "—";
  if (reading.window === null) return tokenCount(reading.used);
  return `${Math.round((reading.used / reading.window) * 100)}%`;
}

/**
 * The band the cell is styled by, or an empty string when there is no window.
 *
 * Absence is deliberately not `comfortable`: a row with no denominator would
 * otherwise be painted the same green as one that is genuinely empty.
 */
export function contextTone(session) {
  const reading = readingOf(session);
  if (!reading || reading.window === null) return "";
  const fraction = reading.used / reading.window;
  if (fraction >= CONTEXT_CRITICAL) return CONTEXT_TONES.critical;
  if (fraction >= CONTEXT_WARN) return CONTEXT_TONES.filling;
  return CONTEXT_TONES.comfortable;
}

/**
 * The hover text, which is where the numbers behind the cell live — and where
 * the reading says what it could not measure.
 *
 * `t` is passed in rather than imported so this module stays free of the
 * catalog and can be tested without one.
 */
export function contextTitle(session, t) {
  const reading = readingOf(session);
  const compactions = compactionCount(session);
  // Compaction history belongs on this cell whether or not a reading exists —
  // an unmeasured session that has compacted three times is a session whose
  // pauses are explained, and that is the fact the operator needs.
  const history = compactions ? ` ${t("context-compactions", { count: compactions })}.` : "";
  if (!reading) return `${t("context-unmeasured")}.${history}`;
  const used = reading.used.toLocaleString("en-US");
  if (reading.window === null) {
    return `${t("context-no-window", { used })}.${history}`;
  }
  return (
    t("context-explained", {
      used,
      window: reading.window.toLocaleString("en-US"),
      percent: Math.round((reading.used / reading.window) * 100),
    }) + `.${history}`
  );
}

/**
 * How many times this session has been compacted, when it has.
 *
 * Shown because compaction is the one long pause the fleet can explain: the
 * agent goes quiet for tens of seconds and its status never moves.
 */
export function compactionCount(session) {
  const value = Number(session?.compactions);
  return Number.isInteger(value) && value > 0 ? value : 0;
}
