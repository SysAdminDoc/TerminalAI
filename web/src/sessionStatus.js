// The shared status contract for fleet rows, summaries, and diagnostics.
//
// Rendering code supplies the translator and quota labeler so this module stays
// executable in isolation. That makes lifecycle semantics testable without
// booting the Tauri shell or searching for function boundaries in its source.

export const STATUS_ORDER = Object.freeze({
  "needs-approval": 8,
  "awaiting-input": 7,
  "needs-you": 6,
  // Sorts with the attention states, not with the busy ones: a rate-limited
  // session renders like a working one and would otherwise sink in a busy fleet.
  "rate-limited": 5.5,
  working: 5,
  thinking: 4,
  idle: 3,
  starting: 2,
  queued: 1,
  unknown: 1,
  exited: 0,
});

export const STATUS_META = Object.freeze({
  "needs-approval": { glyph: "⚠", label: "status-needs-approval", short: "status-needs-approval", tone: "peach" },
  "awaiting-input": { glyph: "?", label: "status-awaiting-input", short: "status-awaiting-input", tone: "yellow" },
  "needs-you": { glyph: "!", label: "status-needs-you", short: "status-needs-you", tone: "peach" },
  "rate-limited": { glyph: "⧗", label: "status-rate-limited", short: "status-rate-limited", tone: "red" },
  working: { glyph: "◒", label: "status-working", short: "status-working", tone: "yellow" },
  thinking: { glyph: "✦", label: "status-thinking", short: "status-thinking", tone: "mauve" },
  idle: { glyph: "·", label: "status-idle", short: "status-idle", tone: "surface2" },
  starting: { glyph: "…", label: "status-starting", short: "status-starting", tone: "sapphire" },
  queued: { glyph: "⏳", label: "status-queued", short: "status-queued", tone: "overlay0" },
  unknown: { glyph: "∅", label: "status-unknown", short: "status-unknown", tone: "overlay0" },
  exited: { glyph: "×", label: "status-exited", short: "status-exited", tone: "overlay0" },
});

export const STATUS_KEYS = Object.freeze(Object.keys(STATUS_META));

// These values mirror the supervisor thresholds in terminalai-core. They are
// exposed so the row and its tests cannot silently drift from the explanation
// shown to the operator.
export const STALL_THRESHOLD_MINUTES = 15;
export const SILENCE_THRESHOLD_MINUTES = 15;

export function createSessionStatus({ t, rateLimitedLabel }) {
  /// What the row shows as "the last thing that happened".
  function lastActivity(session) {
    const message = session?.last_message;
    if (typeof message === "string" && message.trim()) return message;
    return session?.last_line || t("empty-no-output");
  }

  /// The supervisor found the process alive and completely silent. Stronger
  /// evidence than the dwell-based stall flag, so it wins the label.
  function isUnresponsive(session) {
    return session?.health === "unresponsive";
  }

  function restartCount(session) {
    const restarts = Number(session?.restarts);
    return Number.isInteger(restarts) ? restarts : 0;
  }

  function statusLabel(status) {
    const meta = STATUS_META[status];
    return meta ? t(meta.label) : status ?? t("status-unknown");
  }

  function lifecycleLabel(session) {
    if (session?.phase === "preparing") return t("status-preparing");
    // Still nominally working, but for long enough that busy and wedged are no
    // longer the same thing. The supervisor decides this; the row says it.
    if (isUnresponsive(session))
      return t("status-unresponsive", { status: statusLabel(session?.status) });
    if (session?.stalled) return t("status-stalled", { status: statusLabel(session?.status) });
    if (session?.phase === "tearing-down") return t("status-tearing-down");
    // A session the supervisor gave up on and one that ended its own work both
    // used to read "Exited — The process has ended", which is true of a crash
    // loop and of a finished job alike and tells the operator nothing about
    // which they are looking at.
    if (session?.phase === "failed") return t("status-failed", { restarts: restartCount(session) });
    if (session?.phase === "finished") return t("status-finished");
    // Carries which quota tripped and when it reopens, so the row says why the
    // session is going nowhere rather than only that it is.
    if (session?.status === "rate-limited") return rateLimitedLabel(session, t);
    return statusLabel(session?.status);
  }

  /// Why a session ended, when "it ended" is not the whole story.
  function lifecycleDetail(session) {
    if (isUnresponsive(session))
      return t("status-unresponsive-detail", { minutes: SILENCE_THRESHOLD_MINUTES });
    if (session?.stalled) return t("status-stalled-detail", { minutes: STALL_THRESHOLD_MINUTES });
    if (session?.phase === "failed") {
      const code = session?.last_exit_code;
      return Number.isInteger(Number(code))
        ? t("status-failed-detail-code", { restarts: restartCount(session), code: Number(code) })
        : t("status-failed-detail", { restarts: restartCount(session) });
    }
    if (session?.phase === "finished") return t("status-finished-detail");
    return "";
  }

  /// The colour a row's glyph takes. Phase overrides status for terminal states.
  function lifecycleTone(session, meta) {
    if (session?.phase === "failed") return "red";
    // Louder than the yellow a healthy working row gets: the whole point is
    // that it no longer looks like one.
    if (isUnresponsive(session) || session?.stalled) return "peach";
    if (session?.phase === "finished") return "green";
    return meta.tone;
  }

  function metaLabel(meta) {
    return t(meta.label);
  }

  return {
    isUnresponsive,
    lastActivity,
    lifecycleDetail,
    lifecycleLabel,
    lifecycleTone,
    metaLabel,
    restartCount,
    statusLabel,
  };
}
