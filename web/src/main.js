import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { reconcileKeyedRows } from "./fleetRows.js";
import { countMessage, localizeDom, relativeDwell, t } from "./i18n.js";
import { rateLimitTitle, rateLimitedLabel } from "./rateLimit.js";
import "./styles.css";

const WDIO_BUILD = import.meta.env.VITE_TERMINALAI_WDIO === "1";

if (WDIO_BUILD) {
  await import("@wdio/tauri-plugin");
}

const STATUS_ORDER = {
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
};

const STATUS_META = {
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
};
const STATUS_KEYS = Object.keys(STATUS_META);
const PREFLIGHT_META = {
  ok: { glyph: "✓", label: "preflight-ready", tone: "green" },
  warn: { glyph: "!", label: "preflight-needs-attention", tone: "peach" },
  error: { glyph: "×", label: "preflight-unavailable", tone: "red" },
  unsupported: { glyph: "—", label: "preflight-not-applicable", tone: "overlay0" },
};
const RELEASES_ENDPOINT = "https://api.github.com/repos/SysAdminDoc/TerminalAI/releases/latest";
const FALLBACK_APP_VERSION = "0.1.0";

/// What the row shows as "the last thing that happened".
///
/// The transcript message wins when it exists: `last_line` is the tail of a
/// rendered TUI, so a redraw leaves box-drawing characters and cursor moves in
/// it. Falls back to the pty tail while the transcript has not been read yet,
/// because an empty row is worse than an ugly one.
function lastActivity(session) {
  const message = session?.last_message;
  if (typeof message === "string" && message.trim()) return message;
  return session?.last_line || t("empty-no-output");
}

function lifecycleLabel(session) {
  if (session?.phase === "preparing") return t("status-preparing");
  if (session?.phase === "tearing-down") return t("status-tearing-down");
  // Carries which quota tripped and when it reopens, so the row says why the
  // session is going nowhere rather than only that it is.
  if (session?.status === "rate-limited") return rateLimitedLabel(session, t);
  return statusLabel(session?.status);
}

function statusLabel(status) {
  const meta = STATUS_META[status];
  return meta ? t(meta.label) : status ?? t("status-unknown");
}

function metaLabel(meta) {
  return t(meta.label);
}

const state = {
  sessions: [],
  external: [],
  externalError: null,
  focused: null,
  presets: [],
  extraDirs: [],
  attentionOnly: false,
  wideMode: false,
  reviewMode: false,
  reviews: [],
  diagnosticsMode: false,
  logsMode: false,
  logs: [],
  screenReaderMode: false,
  appVersion: null,
  storeQuarantine: null,
  storeQuarantineDismissed: false,
  terminal: null,
  outputChannel: null,
  fitAddon: null,
  webglAddon: null,
  /// Pinned panes render from Rust-side grid snapshots, polled on a timer. They
  /// deliberately do not get xterm instances: one renderer is what lets the
  /// fleet hold ~29 rows, and three more would undo that.
  pinnedTimer: null,
  pinnedGrids: new Map(),
  focusGeneration: 0,
  resizeTimer: null,
  lastSentSize: null,
  previewTimer: null,
  preflight: null,
  preflightMode: false,
  preflightLoading: false,
  preflightReason: null,
  capabilities: {},
  capabilityRequest: 0,
  snapshotLoading: true,
  announcementQueue: new Map(),
  announcementTimer: null,
  orderFreeze: null,
  attentionToasts: new Map(),
  admission: { max_live_sessions: 3, live_sessions: 0, queued_sessions: 0, aggregate_cost_usd: 0, dropped_events: 0 },
};

const $ = (id) => document.getElementById(id);

function terminalBytes(payload) {
  if (payload instanceof ArrayBuffer) return new Uint8Array(payload);
  if (ArrayBuffer.isView(payload)) return new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
  if (Array.isArray(payload)) return Uint8Array.from(payload);
  return new TextEncoder().encode(String(payload ?? ""));
}

// Output arrives asynchronously and a focus switch spans two awaits, so a chunk
// for the session we just left can land after the new one is installed. The
// generation token is bumped on every switch; anything stamped with an older one
// is discarded rather than written into the wrong session's grid.
function writeTerminalBytes(payload, id = state.focused, generation = state.focusGeneration) {
  if (id !== state.focused || generation !== state.focusGeneration) return;
  if (state.terminal) state.terminal.write(terminalBytes(payload));
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function invokeArgs(spec) {
  return { spec, configuredPath: null };
}

function showToast(message, tone = "error") {
  const toast = document.createElement("div");
  toast.className = `toast toast-${tone}`;
  toast.textContent = message;
  $("toast-region").append(toast);
  requestAnimationFrame(() => toast.classList.add("toast-visible"));
  setTimeout(() => {
    toast.classList.remove("toast-visible");
    setTimeout(() => toast.remove(), 240);
  }, 4200);
}

function renderStoreQuarantine() {
  const banner = $("store-quarantine-banner");
  const path = state.storeQuarantine;
  const visible = Boolean(path) && !state.storeQuarantineDismissed;
  banner.classList.toggle("view-hidden", !visible);
  $("store-quarantine-message").textContent = path
    ? t("store-quarantined-detail", { path })
    : "";
}

function showAttentionToast(notification) {
  if (state.attentionToasts.has(notification.dedup_key)) return;
  const session = state.sessions.find((item) => item.id === notification.session_id);
  const meta = STATUS_META[notification.status] ?? STATUS_META["needs-you"];
  const toast = document.createElement("button");
  toast.type = "button";
  toast.className = "toast toast-attention toast-visible";
  toast.textContent = `${session?.name ?? notification.session_id} · ${metaLabel(meta)} · ${folderLabel(notification.group_key)}`;
  toast.title = t("action-focus-terminal");
  toast.addEventListener("click", () => void focusSession(notification.session_id));
  $("toast-region").append(toast);
  state.attentionToasts.set(notification.dedup_key, { toast, sessionId: notification.session_id });
}

function retractAttentionToast(dedupKey) {
  const entry = state.attentionToasts.get(dedupKey);
  if (!entry) return;
  state.attentionToasts.delete(dedupKey);
  entry.toast.classList.remove("toast-visible");
  setTimeout(() => entry.toast.remove(), 240);
}

function systemTimeMs(value) {
  if (typeof value === "number") return value;
  if (value && typeof value.secs_since_epoch === "number") {
    return value.secs_since_epoch * 1000 + Math.floor((value.nanos_since_epoch ?? 0) / 1e6);
  }
  return Date.now();
}

function dwell(value) {
  return relativeDwell(value);
}

function toolProgress(value) {
  const completed = Number(value?.completed);
  const total = Number(value?.total);
  if (!Number.isInteger(completed) || !Number.isInteger(total) || completed < 0 || total <= 0) return "—";
  return `${Math.min(completed, total)}/${total}`;
}

// Number(null) is 0, so a session that has never reported a cost used to render
// "$0.00" — a computed-looking zero is worse than an honest em dash.
function cost(value) {
  if (value === null || value === undefined || value === "") return "—";
  const amount = Number(value);
  return Number.isFinite(amount) ? `$${amount.toFixed(2)}` : "—";
}

function reviewNumber(value) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.floor(number) : 0;
}

function versionTuple(value) {
  const match = String(value ?? "").replace(/^v/i, "").match(/^(\d+)\.(\d+)\.(\d+)/);
  return match ? match.slice(1).map(Number) : null;
}

function isNewerVersion(candidate, current) {
  const next = versionTuple(candidate);
  const installed = versionTuple(current);
  if (!next || !installed) return false;
  for (let index = 0; index < next.length; index += 1) {
    if (next[index] !== installed[index]) return next[index] > installed[index];
  }
  return false;
}

async function checkForUpdates() {
  const button = $("update-check-button");
  if (button.disabled) return;
  button.disabled = true;
  button.querySelector("span").textContent = "Checking…";
  try {
    const current = state.appVersion ?? await invoke("app_version").catch(() => FALLBACK_APP_VERSION);
    state.appVersion = current;
    const response = await fetch(RELEASES_ENDPOINT, {
      headers: {
        Accept: "application/vnd.github+json",
      },
    });
    if (response.status === 404) {
      showToast(`TerminalAI v${current} is the newest published build; no update was installed.`, "success");
      return;
    }
    if (!response.ok) throw new Error(`GitHub returned HTTP ${response.status}`);
    const release = await response.json();
    const latest = String(release.tag_name ?? "").replace(/^v/i, "");
    if (!versionTuple(latest)) throw new Error("the latest release had no semantic version");
    if (isNewerVersion(latest, current)) {
      showToast(`TerminalAI v${latest} is available (installed v${current}). Download it from GitHub; nothing was installed automatically.`, "success");
    } else {
      showToast(`TerminalAI v${current} is up to date; no update was installed.`, "success");
    }
  } catch (error) {
    showToast(`Update check failed: ${error}`);
  } finally {
    button.disabled = false;
    button.querySelector("span").textContent = "Check updates";
  }
}

function diagnosticSource(value) {
  const key = String(value ?? "unknown");
  const localized = t(`source-${key}`);
  if (localized !== `source-${key}`) return localized;
  return key
    .split("-")
    .map((part) => part ? part[0].toUpperCase() + part.slice(1) : part)
    .join(" ");
}

function diagnosticTime(value) {
  const time = systemTimeMs(value);
  return Number.isFinite(time) ? new Date(time).toISOString().replace(".000Z", "Z").replace("T", " ") : "unknown time";
}

function renderDiagnostics() {
  const host = $("diagnostics-host");
  const session = state.sessions.find((item) => item.id === state.focused);
  if (!session) {
    host.innerHTML = `<div class="diagnostics-empty">${escapeHtml(t("empty-focus-diagnostics"))}</div>`;
    return;
  }
  const history = Array.isArray(session.status_history) ? [...session.status_history].reverse() : [];
  const latest = history[0];
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const label = lifecycleLabel(session);
  const source = latest?.source ? diagnosticSource(latest.source) : t("diagnostics-unavailable");
  const timeline = history.length
    ? history.map((entry) => {
      const entryMeta = STATUS_META[entry.to] ?? STATUS_META.exited;
      const from = entry.from ? statusLabel(entry.from) : t("session-created");
      const reason = formatReason(entry.reason, entry.detail);
      return '<li class="diagnostic-event"><span class="diagnostic-event-glyph tone-' + entryMeta.tone + '" aria-hidden="true">' + entryMeta.glyph + '</span><div class="diagnostic-event-body"><div><b>' + escapeHtml(metaLabel(entryMeta)) + '</b><span>' + escapeHtml(t("diagnostics-from", { status: from })) + '</span></div><small>' + escapeHtml(diagnosticSource(entry.source)) + ' · ' + escapeHtml(diagnosticTime(entry.at)) + '</small>' + (reason ? '<p>' + escapeHtml(reason) + '</p>' : '') + '</div></li>';
    }).join("")
    : `<li class="diagnostics-empty">${escapeHtml(t("empty-no-transition-history"))}</li>`;
  host.innerHTML = '<div class="diagnostics-heading"><div><span class="eyebrow">' + escapeHtml(t("diagnostics-why-this-state")) + '</span><h2>' + escapeHtml(session.name) + '</h2><p>' + escapeHtml(session.cwd) + '</p></div><div class="diagnostics-heading-actions"><button type="button" class="button button-quiet" data-diagnostics-action="preflight">' + escapeHtml(t("button-preflight")) + '</button><span class="status-glyph tone-' + meta.tone + '" title="' + escapeHtml(label) + '" aria-hidden="true">' + meta.glyph + '</span></div></div>' +
    '<div class="diagnostics-current"><span>' + escapeHtml(t("diagnostics-current-status")) + '</span><b>' + escapeHtml(label) + '</b><span>' + escapeHtml(t("diagnostics-for", { dwell: dwell(session.status_since) })) + ' · ' + escapeHtml(t("diagnostics-source", { source })) + '</span></div>' +
    '<ol class="diagnostics-timeline">' + timeline + '</ol>';
}

function formatReason(reason, legacyDetail = null) {
  if (!reason?.kind) return legacyDetail || t("reason-unknown");
  const args = reason.args ?? {};
  const status = args.status ? statusLabel(args.status) : t("status-unknown");
  const messageArgs = { ...args, status, code: args.code ?? "unknown" };
  const id = `reason-${reason.kind}`;
  return t(id, messageArgs) === id ? (legacyDetail || t("reason-unknown")) : t(id, messageArgs);
}

function logTime(value) {
  const time = systemTimeMs(value);
  return Number.isFinite(time) ? new Date(time).toISOString().replace(".000Z", "Z").replace("T", " ") : "unknown time";
}

function renderLogs() {
  const host = $("logs-host");
  if (!state.logs.length) {
    host.innerHTML = `<div class="logs-empty">${escapeHtml(t("empty-no-daemon-records"))}</div>`;
    return;
  }
  const rows = [...state.logs].reverse().map((entry) => {
    const fields = Object.entries(entry.fields ?? {})
      .filter(([key]) => ["session_id", "agent", "cwd"].includes(key))
      .map(([key, value]) => `<span>${escapeHtml(key)}=${escapeHtml(value)}</span>`)
      .join("");
    return `<li class="log-event"><div class="log-event-heading"><b>${escapeHtml(entry.level ?? "INFO")}</b><span>${escapeHtml(entry.target ?? "terminalai")}</span><time>${escapeHtml(logTime(entry.at))}</time></div><p>${escapeHtml(entry.message ?? "")}</p>${fields ? `<div class="log-event-fields">${fields}</div>` : ""}</li>`;
  }).join("");
  host.innerHTML = '<div class="logs-heading"><div><span class="eyebrow">' + escapeHtml(t("logs-control-plane")) + '</span><h2>' + escapeHtml(t("logs-daemon-records")) + '</h2><p>' + escapeHtml(t("logs-latest-retained")) + '</p></div><span class="status-glyph tone-sapphire" aria-hidden="true">≋</span></div><ol class="logs-list">' + rows + '</ol>';
}

function syncDiagnosticsVisibility() {
  const diagnostics = state.diagnosticsMode;
  const logs = state.logsMode;
  $("terminal-host").classList.toggle("view-hidden", diagnostics || logs);
  $("diagnostics-host").classList.toggle("view-hidden", !diagnostics);
  $("logs-host").classList.toggle("view-hidden", !logs);
  $("diagnostics-toggle").setAttribute("aria-pressed", String(diagnostics));
  $("diagnostics-toggle").classList.toggle("row-action-active", diagnostics);
  $("diagnostics-toggle").textContent = diagnostics ? "▣" : "?";
  $("logs-toggle").setAttribute("aria-pressed", String(logs));
  $("logs-toggle").classList.toggle("row-action-active", logs);
  $("logs-toggle").textContent = logs ? "▣" : "≋";
  if (diagnostics) renderDiagnostics();
  if (logs) renderLogs();
}

function setDiagnosticsMode(active) {
  state.diagnosticsMode = active;
  if (active) state.logsMode = false;
  syncDiagnosticsVisibility();
}

function setLogsMode(active) {
  state.logsMode = active;
  if (active) state.diagnosticsMode = false;
  syncDiagnosticsVisibility();
}

function setScreenReaderMode(active) {
  state.screenReaderMode = Boolean(active);
  if (state.terminal) state.terminal.options.screenReaderMode = state.screenReaderMode;
  const toggle = $("screen-reader-toggle");
  toggle.setAttribute("aria-pressed", String(state.screenReaderMode));
  toggle.classList.toggle("row-action-active", state.screenReaderMode);
  toggle.title = state.screenReaderMode
    ? t("screen-reader-disable")
    : t("screen-reader-enable");
}

function appendLogs(entries) {
  if (!Array.isArray(entries)) return;
  state.logs.push(...entries);
  state.logs = state.logs.slice(-256);
  if (state.logsMode) renderLogs();
}

function syncPreflightVisibility() {
  const active = state.preflightMode;
  ["fleet-state-strip", "column-labels", "fleet-list", "fleet-order-notice", "empty-state", "review-view"].forEach((id) => {
    $(id).classList.toggle("view-hidden", active);
  });
  $("preflight-view").classList.toggle("view-hidden", !active);
  $("preflight-toggle").setAttribute("aria-pressed", String(active));
  $("preflight-toggle").classList.toggle("wide-toggle-active", active);
  if (active) renderPreflight();
}

function setPreflightMode(active) {
  state.preflightMode = active;
  if (active) {
    state.reviewMode = false;
    state.diagnosticsMode = false;
    state.logsMode = false;
  }
  syncPreflightVisibility();
  syncReviewVisibility();
  syncDiagnosticsVisibility();
  if (active && !state.preflight) void loadPreflight();
  if (!active) renderRows();
}

function preflightChecksNeedAttention(report) {
  return (report?.checks ?? []).some((check) => !["ok", "unsupported"].includes(check.state));
}

function renderPreflight() {
  const report = state.preflight;
  const checks = Array.isArray(report?.checks) ? report.checks : [];
  const attention = checks.filter((check) => !["ok", "unsupported"].includes(check.state)).length;
  $("preflight-summary").textContent = state.preflightLoading
    ? t("preflight-checking")
    : state.preflightReason
      ? state.preflightReason
      : attention
        ? countMessage("count-check", attention)
        : t("preflight-all-ready");
  $("preflight-list").innerHTML = checks.map((check) => {
    const meta = PREFLIGHT_META[check.state] ?? PREFLIGHT_META.error;
    const detail = check.detail ? `<small>${escapeHtml(check.detail)}</small>` : "";
    const fixLabel = check.can_fix ? t("button-fix") : t("button-fix-unavailable");
    return `<article class="preflight-row" role="listitem"><span class="status-glyph tone-${escapeHtml(meta.tone)}" title="${escapeHtml(metaLabel(meta))}" aria-hidden="true">${escapeHtml(meta.glyph)}</span><div class="preflight-copy"><div><b>${escapeHtml(check.label)}</b><span>${escapeHtml(metaLabel(meta))}</span></div><strong>${escapeHtml(check.detected)}</strong>${detail}</div><div class="preflight-actions"><button type="button" class="button button-secondary" data-preflight-action="fix" data-preflight-id="${escapeHtml(check.id)}"${check.can_fix ? "" : " disabled"} aria-label="${escapeHtml(fixLabel)} ${escapeHtml(check.label)}">${escapeHtml(fixLabel)}</button><button type="button" class="button button-quiet" data-preflight-action="recheck" data-preflight-id="${escapeHtml(check.id)}" aria-label="${escapeHtml(t("button-recheck"))} ${escapeHtml(check.label)}">${escapeHtml(t("button-recheck"))}</button></div></article>`;
  }).join("");
}

async function loadPreflight(show = false) {
  if (show) {
    state.preflightMode = true;
    syncPreflightVisibility();
  }
  state.preflightLoading = true;
  renderPreflight();
  try {
    const report = await invoke("preflight_report");
    state.preflight = report;
    state.preflightReason = null;
    if (!show && preflightChecksNeedAttention(report)) state.preflightMode = true;
  } catch (error) {
    state.preflightReason = `Preflight could not run: ${error}`;
    state.preflightMode = true;
  } finally {
    state.preflightLoading = false;
    syncPreflightVisibility();
    syncReviewVisibility();
    if (!state.preflightMode) renderRows();
  }
}

async function handlePreflightAction(action, id, button) {
  if (button) button.disabled = true;
  try {
    if (action === "fix") {
      await invoke("preflight_fix", { kind: id });
      showToast(`${id} preflight fix applied`, "success");
    }
    await loadPreflight(true);
    if (id === "daemon" || action === "recheck") {
      try {
        await loadSnapshot();
        if (!preflightChecksNeedAttention(state.preflight)) setPreflightMode(false);
      } catch (_) {
        // The preflight panel remains visible with the daemon check's detail.
      }
    }
  } catch (error) {
    state.preflightReason = `Could not ${action} ${id}: ${error}`;
    renderPreflight();
    showToast(state.preflightReason);
  } finally {
    if (button) button.disabled = false;
  }
}

function syncReviewVisibility() {
  const hidden = state.reviewMode || state.preflightMode;
  ["fleet-state-strip", "column-labels", "fleet-list", "empty-state"].forEach((id) => {
    $(id).classList.toggle("view-hidden", hidden);
  });
  $("review-view").classList.toggle("view-hidden", !state.reviewMode || state.preflightMode);
  $("review-toggle").setAttribute("aria-pressed", String(hidden));
  $("review-toggle").classList.toggle("wide-toggle-active", state.reviewMode && !state.preflightMode);
  $("review-toggle").textContent = state.reviewMode && !state.preflightMode ? t("button-fleet") : t("button-review");
}

function renderReview() {
  const entries = Array.isArray(state.reviews) ? state.reviews : [];
  const pending = entries.filter((entry) => !entry.reviewed).length;
  const conflicts = entries.filter((entry) => (entry.conflicts?.length ?? 0) > 0 || reviewNumber(entry.conflict_markers) > 0).length;
  const timedOut = entries.filter((entry) => entry.timed_out === true).length;
  $("review-summary").textContent = `${countMessage("count-session", entries.length)} · ${countMessage("count-pending", pending)} · ${countMessage("count-conflict", conflicts)}${timedOut ? ` · ${countMessage("count-timed-out", timedOut)}` : ""}`;
  $("review-empty").classList.toggle("view-hidden", entries.length > 0);
  $("review-list").innerHTML = entries.map(renderReviewEntry).join("");
}

function renderReviewEntry(entry) {
  const conflicts = Array.isArray(entry.conflicts) ? entry.conflicts : [];
  const markers = reviewNumber(entry.conflict_markers);
  const additions = reviewNumber(entry.additions);
  const deletions = reviewNumber(entry.deletions);
  const files = reviewNumber(entry.files_changed);
  const reviewCost = reviewNumber(entry.review_cost);
  const agent = entry.agent === "codex" ? "Codex" : "Claude Code";
  const status = entry.timed_out ? t("review-status-timed-out") : (entry.reviewed ? t("review-status-reviewed") : t("review-status-pending"));
  const conflictDetails = conflicts.length
    ? "<ul>" + conflicts.map((path) => "<li><code>" + escapeHtml(path) + "</code></li>").join("") + "</ul>"
    : "";
  const conflictMarkup = conflicts.length || markers
    ? '<div class="review-conflict" role="alert"><strong>' + escapeHtml(t("review-conflict-markers")) + '</strong><span>' + escapeHtml(countMessage("review-conflicted-file", conflicts.length)) + (markers ? " · " + escapeHtml(t("review-marker-lines", { count: markers })) : "") + "</span>" + conflictDetails + "</div>"
    : "";
  const errorMarkup = entry.error ? '<div class="review-error" role="alert">' + escapeHtml(entry.error) + "</div>" : "";
  const diffMarkup = entry.diff
    ? '<details class="review-diff" ' + (conflicts.length || markers ? "open" : "") + "><summary>" + escapeHtml(t("review-show-diff")) + (entry.diff_truncated ? " · " + escapeHtml(t("review-truncated")) : "") + "</summary><pre>" + escapeHtml(entry.diff) + "</pre></details>"
    : '<div class="review-no-diff">' + escapeHtml(t("review-no-diff")) + "</div>";
  const actionMarkup = entry.reviewed
    ? '<span class="reviewed-label">✓ ' + escapeHtml(t("review-reviewed")) + '</span>'
    : entry.error
      ? ""
      : '<button type="button" class="button button-secondary review-mark" data-review-action="mark-reviewed" data-review-id="' + escapeHtml(entry.session_id) + '">' + escapeHtml(t("review-mark-reviewed")) + '</button>';
  // Landing is offered only when this collection is trustworthy. A timed-out or
  // errored review describes a tree nobody read, and conflicts are refused by
  // the gate anyway — offering the button there would teach the operator that
  // the button sometimes does nothing.
  const canLand = !entry.error && !entry.timed_out && !conflicts.length && files > 0;
  const landMarkup = canLand
    ? '<button type="button" class="button button-secondary review-land" data-review-action="land" data-review-id="' + escapeHtml(entry.session_id) + '" data-review-cwd="' + escapeHtml(entry.cwd) + '" title="' + escapeHtml(t("review-land-hint")) + '">' + escapeHtml(t("review-land")) + "</button>"
    : "";
  return '<article class="review-entry' + (entry.reviewed ? " review-entry-reviewed" : "") + (entry.timed_out ? " review-entry-timeout" : "") + '" role="listitem">' +
    '<div class="review-entry-heading"><div><h3>' + escapeHtml(entry.name) + '</h3><div class="review-repo"><span>' + escapeHtml(folderLabel(entry.cwd)) + '</span><span>' + escapeHtml(agent) + '</span><code>' + escapeHtml(entry.session_id) + '</code></div></div><div class="review-entry-action">' + landMarkup + actionMarkup + "</div></div>" +
    '<div class="review-metrics"><span>' + escapeHtml(countMessage("count-file", files)) + '</span><span class="review-additions">+' + additions + '</span><span class="review-deletions">−' + deletions + '</span><span>' + escapeHtml(t("review-cost", { cost: reviewCost })) + '</span><span class="review-state">' + escapeHtml(status) + "</span></div>" +
    conflictMarkup + errorMarkup + diffMarkup + "</article>";
}

function ports(value) {
  const assigned = Array.isArray(value)
    ? value.map(Number).filter((port) => Number.isInteger(port) && port > 0 && port <= 65535)
    : [];
  if (!assigned.length) return "—";
  if (assigned.length > 1 && assigned.every((port, index) => index === 0 || port === assigned[index - 1] + 1)) {
    return String(assigned[0]) + "–" + String(assigned.at(-1));
  }
  return assigned.join(", ");
}

function folderLabel(path) {
  const parts = String(path ?? "").split(/[\\/]/).filter(Boolean);
  return parts.at(-1) ?? path ?? "—";
}

function sortedSessions() {
  return [...state.sessions].sort((a, b) => {
    const status = (STATUS_ORDER[b.status] ?? 0) - (STATUS_ORDER[a.status] ?? 0);
    if (status !== 0) return status;
    return systemTimeMs(a.status_since) - systemTimeMs(b.status_since);
  });
}

function renderSummary() {
  // Matches the daemon's admission count, which excludes rate-limited sessions:
  // they hold no slot, so counting them as live would contradict the queue.
  const live = state.sessions.filter((session) => !["exited", "queued", "rate-limited"].includes(session.status)).length;
  const limited = state.sessions.filter((session) => session.status === "rate-limited");
  const queued = state.sessions.filter((session) => session.status === "queued").length;
  const needsYou = state.sessions.filter((session) => ["needs-you", "needs-approval", "awaiting-input"].includes(session.status)).length;
  const working = state.sessions.filter((session) => ["working", "thinking"].includes(session.status)).length;
  const reporting = state.sessions.filter((session) => session.cost_usd !== null && session.cost_usd !== undefined);
  const spend = reporting.reduce((total, session) => total + (Number(session.cost_usd) || 0), 0);
  const spendLabel = reporting.length ? `$${spend.toFixed(2)}` : "—";
  // A price table has a date; a figure priced against an unnamed table cannot be
  // checked. Say which one produced this number.
  const pricingVersion = state.admission.pricing_version || "no price table";
  const spendTitle = reporting.length
    ? t("pricing-reporting", { pricing: pricingVersion, reporting: reporting.length, sessions: state.sessions.length })
    : t("pricing-none", { pricing: pricingVersion });
  const maxLive = state.admission.max_live_sessions ?? 3;
  const limitedSummary = limited.length
    ? `<span class="summary-separator">/</span><span class="summary-item summary-limited" title="${escapeHtml(rateLimitTitle(limited, t))}">${escapeHtml(countMessage("count-rate-limited", limited.length))}</span>`
    : "";
  $("fleet-summary").innerHTML = `<span class="summary-item"><b>${live}/${maxLive}</b> ${escapeHtml(t("fleet-live"))}</span><span class="summary-separator">/</span><span class="summary-item">${escapeHtml(countMessage("count-queued", queued))}</span><span class="summary-separator">/</span><span class="summary-item summary-attention">${escapeHtml(countMessage("count-needs-you", needsYou))}</span><span class="summary-separator">/</span><span class="summary-item">${escapeHtml(countMessage("count-active", working))}</span>${limitedSummary}<span class="summary-separator">/</span><span class="summary-item" title="${escapeHtml(spendTitle)}"><b>${spendLabel}</b> ${escapeHtml(t("fleet-spent"))}</span>`;
  const droppedEvents = Number(state.admission.dropped_events) || 0;
  $("fleet-count").textContent = droppedEvents
    ? `${countMessage("count-session", state.sessions.length)} · ${t("event-drops", { count: droppedEvents })}`
    : t("tracked-sessions", { count: state.sessions.length });
  const counts = Object.fromEntries(STATUS_KEYS.map((status) => [status, 0]));
  for (const session of state.sessions) {
    if (session.status in counts) counts[session.status] += 1;
  }
  $("fleet-state-strip").innerHTML = STATUS_KEYS.map((status) => {
    const meta = STATUS_META[status];
    return `<span class="state-chip tone-${escapeHtml(meta.tone)}" role="listitem" title="${escapeHtml(metaLabel(meta))}: ${escapeHtml(counts[status])}" aria-label="${escapeHtml(metaLabel(meta))}: ${escapeHtml(counts[status])}"><span class="state-chip-glyph" aria-hidden="true">${meta.glyph}</span><b>${counts[status]}</b><span>${escapeHtml(t(meta.short))}</span></span>`;
  }).join("");
}

/// How often pinned panes re-read their grid.
///
/// Slower than the focused terminal's live stream on purpose: a pinned pane is
/// for noticing that something changed, not for reading along. Each read is one
/// small daemon call returning already-parsed text.
const PINNED_POLL_MS = 1000;
/// The daemon refuses a fourth pin; the UI states the same number so the two
/// cannot drift apart silently.
const MAX_PINNED = 3;

function pinnedSessions() {
  return state.sessions.filter((session) => session.pinned).slice(0, MAX_PINNED);
}

/// Render the split view beneath the focused terminal.
///
/// Panes are keyed by session id and reconciled rather than rebuilt, so a
/// snapshot arriving for one pane cannot scroll or blank another.
function renderPinnedSplit() {
  const host = $("pinned-split");
  const pinned = pinnedSessions();
  host.hidden = pinned.length === 0;
  host.classList.toggle("pinned-split-active", pinned.length > 0);
  if (!pinned.length) {
    host.replaceChildren();
    state.pinnedGrids.clear();
    return;
  }
  reconcileKeyedRows(
    host,
    pinned,
    (session) => session.id,
    (session) => {
      const pane = document.createElement("article");
      pane.className = "pinned-pane";
      pane.dataset.id = session.id;
      pane.innerHTML =
        '<header class="pinned-pane-head"><span class="pinned-pane-name"></span>' +
        '<span class="pinned-pane-status"></span></header><pre class="pinned-pane-grid"></pre>';
      return pane;
    },
    (pane, session) => {
      pane.querySelector(".pinned-pane-name").textContent = session.name;
      const status = pane.querySelector(".pinned-pane-status");
      status.textContent = lifecycleLabel(session);
      status.className = `pinned-pane-status tone-${(STATUS_META[session.status] ?? STATUS_META.exited).tone}`;
      const grid = pane.querySelector(".pinned-pane-grid");
      const snapshot = state.pinnedGrids.get(session.id);
      // Until the first snapshot lands the pane says so rather than showing an
      // empty box that reads as "this session printed nothing".
      grid.textContent = snapshot ?? t("pinned-waiting");
      grid.classList.toggle("pinned-pane-grid-waiting", !snapshot);
    },
    () => false,
  );
}

/// Read each pinned session's grid and redraw the split view.
async function refreshPinnedGrids() {
  const pinned = pinnedSessions();
  if (!pinned.length) return;
  const results = await Promise.all(
    pinned.map(async (session) => {
      try {
        const grid = await invoke("grid_snapshot", { id: session.id });
        // Trailing blank rows are most of an idle grid; dropping them keeps a
        // pane the height of its content instead of a fixed 40 lines.
        const lines = Array.isArray(grid?.lines) ? [...grid.lines] : [];
        while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
        return [session.id, lines.join("\n")];
      } catch {
        // A session that exited between the poll and the read is not an error
        // worth a toast; the row already says so.
        return [session.id, null];
      }
    }),
  );
  let changed = false;
  for (const [id, text] of results) {
    if (text === null) continue;
    if (state.pinnedGrids.get(id) !== text) {
      state.pinnedGrids.set(id, text);
      changed = true;
    }
  }
  // Drop grids for sessions that are no longer pinned, so unpinning and
  // repinning does not show a stale frame from minutes ago.
  const live = new Set(pinned.map((session) => session.id));
  for (const id of [...state.pinnedGrids.keys()]) {
    if (!live.has(id)) {
      state.pinnedGrids.delete(id);
      changed = true;
    }
  }
  if (changed) renderPinnedSplit();
}

function startPinnedPolling() {
  if (state.pinnedTimer) return;
  state.pinnedTimer = setInterval(() => void refreshPinnedGrids(), PINNED_POLL_MS);
}

function renderRows() {
  syncReviewVisibility();
  renderSummary();
  renderPinnedSplit();
  const filter = $("filter-input").value.trim().toLowerCase();
  const desiredSessions = sortedSessions().filter((session) => {
    if (state.attentionOnly && !isAttention(session)) return false;
    if (!filter) return true;
    return [session.name, session.cwd, folderLabel(session.cwd), session.branch, session.agent, session.model, session.status, session.phase, lifecycleLabel(session), lastActivity(session), toolProgress(session.tool_progress), session.restarts, ports(session.ports)]
      .filter(Boolean)
      .join(" ")
      .toLowerCase()
      .includes(filter);
  });
  const pendingPriorityMoves = pendingPriorityChanges(desiredSessions);
  if (state.orderFreeze) state.orderFreeze.pending = pendingPriorityMoves;
  const sessions = applyFrozenOrder(desiredSessions);
  renderOrderNotice(pendingPriorityMoves);
  const list = $("fleet-list");
  $("empty-state").classList.toggle("empty-state-hidden", state.sessions.length > 0);
  list.classList.toggle("fleet-list-hidden", state.sessions.length === 0);
  list.classList.toggle("fleet-list-wide", state.wideMode);
  $("wide-toggle").setAttribute("aria-pressed", String(state.wideMode));
  $("wide-toggle").textContent = state.wideMode ? "Compact" : "Wide";
  $("wide-toggle").classList.toggle("wide-toggle-active", state.wideMode);
  const rovingId = sessions.find((session) => session.id === state.focused)?.id ?? sessions[0]?.id ?? null;
  reconcileKeyedRows(
    list,
    sessions,
    (session) => session.id,
    createFleetRow,
    (row, session) => updateFleetRow(
      row,
      session,
      sessions.findIndex((item) => item.id === session.id) + 1,
      sessions.length,
      rovingId,
    ),
    (row) => state.sessions.some((session) => session.id === row.dataset.id)
      && (row.contains(document.activeElement) || Boolean(row.querySelector("input[data-reply]")?.value)),
  );
}

function renderSnapshotLoading() {
  $("fleet-loading").classList.toggle("view-hidden", !state.snapshotLoading);
}

function currentFleetOrder() {
  return Array.from($("fleet-list").querySelectorAll('[role="option"]'))
    .filter((row) => !row.hidden)
    .map((row) => row.dataset.id)
    .filter(Boolean);
}

function beginFleetOrderFreeze() {
  if (state.orderFreeze) return;
  const ids = currentFleetOrder();
  if (!ids.length) return;
  const focusedRow = document.activeElement?.closest?.('#fleet-list [role="option"]');
  state.orderFreeze = {
    ids,
    focusId: focusedRow?.dataset.id ?? state.focused,
    pending: 0,
  };
}

function fleetListIsInteracting() {
  const list = $("fleet-list");
  return Boolean(list.matches(":hover") || list.contains(document.activeElement));
}

function releaseFleetOrderFreeze() {
  setTimeout(() => {
    if (fleetListIsInteracting() || !state.orderFreeze || state.orderFreeze.pending) return;
    state.orderFreeze = null;
    renderRows();
  }, 0);
}

function pendingPriorityChanges(desiredSessions) {
  if (!state.orderFreeze) return 0;
  const desiredIds = desiredSessions.map((session) => session.id);
  const frozenIds = state.orderFreeze.ids;
  const frozenSet = new Set(frozenIds);
  const desiredSet = new Set(desiredIds);
  const desiredCommon = desiredIds.filter((id) => frozenSet.has(id));
  const frozenCommon = frozenIds.filter((id) => desiredSet.has(id));
  const desiredIndex = new Map(desiredCommon.map((id, index) => [id, index]));
  const frozenIndex = new Map(frozenCommon.map((id, index) => [id, index]));
  let changed = desiredIds.filter((id) => !frozenSet.has(id)).length;
  for (const id of desiredCommon) {
    if (desiredIndex.get(id) !== frozenIndex.get(id)) changed += 1;
  }
  return changed;
}

function applyFrozenOrder(desiredSessions) {
  if (!state.orderFreeze) return desiredSessions;
  const byId = new Map(desiredSessions.map((session) => [session.id, session]));
  const frozen = state.orderFreeze.ids.map((id) => byId.get(id)).filter(Boolean);
  const frozenSet = new Set(frozen.map((session) => session.id));
  return [...frozen, ...desiredSessions.filter((session) => !frozenSet.has(session.id))];
}

function renderOrderNotice(changedCount) {
  const notice = $("fleet-order-notice");
  const visible = changedCount > 0 && !state.reviewMode;
  notice.classList.toggle("view-hidden", !visible);
  if (!visible) return;
  $("fleet-order-message").textContent = `${changedCount} session${changedCount === 1 ? "" : "s"} changed priority`;
}

function applyFleetOrder() {
  const focusId = state.orderFreeze?.focusId;
  state.orderFreeze = null;
  renderRows();
  if (!focusId) return;
  const row = Array.from($("fleet-list").querySelectorAll('[role="option"]'))
    .find((candidate) => candidate.dataset.id === focusId);
  row?.focus({ preventScroll: true });
}

function createFleetRow(session) {
  const template = document.createElement("template");
  template.innerHTML = renderRow(session).trim();
  const row = template.content.firstElementChild;
  bindFleetRow(row);
  return row;
}

function bindFleetRow(row) {
  row.addEventListener("click", () => focusSession(row.dataset.id));
  row.addEventListener("keydown", (event) => {
    if (event.target !== row) return;
    if (["ArrowDown", "ArrowRight", "ArrowUp", "ArrowLeft", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      moveFleetRow(row, ["ArrowDown", "ArrowRight"].includes(event.key) ? 1 : ["ArrowUp", "ArrowLeft"].includes(event.key) ? -1 : event.key === "Home" ? "first" : "last");
      return;
    }
    if (!["Enter", " "].includes(event.key)) return;
    event.preventDefault();
    void focusSession(row.dataset.id);
  });
  for (const button of row.querySelectorAll("button[data-action]")) {
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      rowAction(button.dataset.action, row.dataset.id, row);
    });
  }
  const reply = row.querySelector("input[data-reply]");
  reply?.addEventListener("click", (event) => event.stopPropagation());
  reply?.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    rowAction("reply", row.dataset.id, row);
  });
}

function moveFleetRow(row, direction) {
  const rows = Array.from($("fleet-list").querySelectorAll('[role="option"]')).filter((candidate) => !candidate.hidden);
  const index = rows.indexOf(row);
  if (index < 0 || rows.length < 2) return;
  const targetIndex = direction === "first"
    ? 0
    : direction === "last"
      ? rows.length - 1
      : Math.max(0, Math.min(rows.length - 1, index + direction));
  const target = rows[targetIndex];
  if (!target || target === row) return;
  row.tabIndex = -1;
  target.tabIndex = 0;
  target.focus({ preventScroll: false });
  void focusSession(target.dataset.id);
}

function updateFleetRow(row, session, position = 1, setSize = 1, rovingId = state.focused) {
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const label = lifecycleLabel(session);
  const active = session.id === state.focused;
  const unread = Boolean(session.unread);
  const agentLabel = session.agent === "codex" ? "CX" : "CC";
  const model = session.model || "default";
  const effort = session.effort || "—";
  const repo = folderLabel(session.cwd);
  const branch = session.branch || "—";
  const progress = toolProgress(session.tool_progress);
  const restartCount = Number.isInteger(Number(session.restarts)) ? Number(session.restarts) : 0;
  const lastLine = lastActivity(session);
  const pinLabel = session.pinned ? t("action-unpin") : t("action-pin");
  const sessionLabel = `${session.name}, ${label}, ${repo}, ${branch}, ${t("action-tool-progress")} ${progress}, ${restartCount} ${t("action-restart-count")}, ${t("action-allocated-ports")} ${ports(session.ports)}`;

  row.className = `fleet-row${active ? " row-focused" : ""}${unread ? " row-unread" : ""}`;
  row.dataset.id = session.id;
  row.setAttribute("aria-label", sessionLabel);
  row.setAttribute("aria-posinset", String(position));
  row.setAttribute("aria-setsize", String(setSize));
  row.setAttribute("aria-selected", String(active));
  row.tabIndex = session.id === rovingId ? 0 : -1;

  const glyph = row.querySelector(".status-glyph");
  glyph.className = `status-glyph tone-${meta.tone}`;
  glyph.title = label;
  glyph.textContent = meta.glyph;

  row.querySelector(".row-name-text").textContent = session.name;
  const unreadDot = row.querySelector(".unread-dot");
  if (unread && !unreadDot) {
    const dot = document.createElement("span");
    dot.className = "unread-dot";
    dot.title = t("action-unread-attention");
    row.querySelector(".row-name").append(dot);
  } else if (!unread && unreadDot) {
    unreadDot.remove();
  }
  row.querySelector(".row-repo").textContent = repo;
  row.querySelector(".row-branch").textContent = branch;
  row.querySelector(".row-status-label").textContent = label;
  const portBadge = row.querySelector(".row-ports");
  portBadge.textContent = `${t("action-allocated-ports")} ${ports(session.ports)}`;

  const agentBadge = row.querySelector(".agent-badge");
  agentBadge.className = `agent-badge agent-${session.agent}`;
  agentBadge.title = session.agent === "codex" ? "Codex" : "Claude Code";
  agentBadge.setAttribute("aria-label", agentBadge.title);
  agentBadge.textContent = agentLabel;
  row.querySelector(".row-progress b").textContent = progress;
  row.querySelector(".row-restarts").textContent = `↻ ${restartCount}`;
  row.querySelector(".row-dwell > span").textContent = dwell(session.status_since);
  const lastLineElement = row.querySelector(".row-last-line");
  lastLineElement.title = lastLine;
  lastLineElement.textContent = lastLine;

  const pin = row.querySelector('[data-action="pin"]');
  pin.classList.toggle("row-action-active", session.pinned);
  pin.title = pinLabel;
  pin.setAttribute("aria-label", `${pinLabel} ${session.name}`);
  pin.textContent = session.pinned ? "◆" : "◇";
  const focus = row.querySelector('[data-action="focus"]');
  focus.setAttribute("aria-label", t("action-focus-session", { name: session.name }));
  const revive = row.querySelector('[data-action="revive"]');
  revive.hidden = !(session.status === "exited" && session.resume_id);
  revive.title = t("action-revive", { name: session.name });
  revive.setAttribute("aria-label", t("action-revive", { name: session.name }));
  const archive = row.querySelector('[data-action="archive"]');
  archive.hidden = session.status !== "exited";
  archive.setAttribute("aria-label", t("action-archive", { name: session.name }));
  const stop = row.querySelector('[data-action="kill"]');
  stop.hidden = session.status === "exited";
  const stopLabel = session.status === "queued" ? t("action-cancel-queued") : t("action-stop", { name: session.name });
  stop.title = stopLabel;
  stop.setAttribute("aria-label", stopLabel);

  const wideMeta = row.querySelector(".row-wide-meta");
  wideMeta.hidden = !state.wideMode;
  wideMeta.querySelector("[data-row-model]").textContent = model;
  wideMeta.querySelector("[data-row-effort]").textContent = effort;
  wideMeta.querySelector("[data-row-cost]").textContent = cost(session.cost_usd);

  const reply = row.querySelector(".row-reply");
  const replyInput = row.querySelector("input[data-reply]");
  reply.hidden = !isAttention(session) && !replyInput.value;
  replyInput.setAttribute("aria-label", t("action-reply", { name: session.name }));
  row.querySelector(".row-reply-send").setAttribute("aria-label", t("action-send-reply", { name: session.name }));
}

function isAttention(session) {
  return ["needs-approval", "awaiting-input", "needs-you"].includes(session.status);
}

function announceStatusChange(session, previousStatus) {
  if (!previousStatus || previousStatus === session.status) return;
  if (!isAttention(session)) return;
  state.announcementQueue.set(session.id, {
    name: session.name,
    label: lifecycleLabel(session),
  });
  if (state.announcementTimer !== null) return;
  state.announcementTimer = setTimeout(flushAnnouncements, 2000);
}

function flushAnnouncements() {
  state.announcementTimer = null;
  if (!state.announcementQueue.size) return;
  const entries = Array.from(state.announcementQueue.values());
  state.announcementQueue.clear();
  const message = entries.length === 1
    ? t("announcement-one", { name: entries[0].name, status: entries[0].label })
    : t("announcement-many", { count: entries.length, names: entries.map((entry) => entry.name).join(", ") });
  const announcer = $("fleet-announcer");
  announcer.textContent = "";
  setTimeout(() => {
    announcer.textContent = message;
  }, 0);
}

function renderRow(session) {
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const label = lifecycleLabel(session);
  const active = session.id === state.focused ? " row-focused" : "";
  const unread = session.unread ? " row-unread" : "";
  const agentLabel = session.agent === "codex" ? "CX" : "CC";
  const model = session.model || "default";
  const effort = session.effort || "—";
  const repo = folderLabel(session.cwd);
  const branch = session.branch || "—";
  const progress = toolProgress(session.tool_progress);
  const restartCount = Number.isInteger(Number(session.restarts)) ? Number(session.restarts) : 0;
  const lastLine = lastActivity(session);
  const pinLabel = session.pinned ? t("action-unpin") : t("action-pin");
  const reviveHidden = session.status === "exited" && session.resume_id ? "" : " hidden";
  const archiveHidden = session.status === "exited" ? "" : " hidden";
  const stopHidden = session.status === "exited" ? " hidden" : "";
  const stopLabel = session.status === "queued" ? t("action-cancel-queued") : t("action-stop", { name: session.name });
  const replyHidden = isAttention(session) ? "" : " hidden";
  const wideHidden = state.wideMode ? "" : " hidden";
  const portsLabel = ports(session.ports);
  const accessibleLabel = `${session.name}, ${label}, ${repo}, ${branch}, ${t("action-tool-progress")} ${progress}, ${restartCount} ${t("action-restart-count")}, ${t("action-allocated-ports")} ${portsLabel}`;
  return `<article class="fleet-row${escapeHtml(active)}${escapeHtml(unread)}" data-id="${escapeHtml(session.id)}" role="option" tabindex="-1" aria-posinset="1" aria-setsize="1" aria-selected="false" aria-keyshortcuts="Enter Space ArrowUp ArrowDown Home End" aria-label="${escapeHtml(accessibleLabel)}">
    <div class="row-identity"><span class="status-glyph tone-${escapeHtml(meta.tone)}" title="${escapeHtml(label)}" aria-hidden="true">${meta.glyph}</span><div class="row-name-wrap"><div class="row-name"><span class="row-name-text">${escapeHtml(session.name)}</span>${session.unread ? `<span class="unread-dot" title="${escapeHtml(t("action-unread-attention"))}"></span>` : ""}</div><div class="row-folder"><span class="row-repo" title="${escapeHtml(t("action-repository"))}">${escapeHtml(repo)}</span><span class="row-branch" title="${escapeHtml(t("action-branch"))}">${escapeHtml(branch)}</span><span class="row-status-label">${escapeHtml(label)}</span><span class="row-ports" title="${escapeHtml(t("action-allocated-ports"))}">${escapeHtml(t("action-allocated-ports"))} ${escapeHtml(portsLabel)}</span></div></div></div>
    <div class="row-metrics"><span class="agent-badge agent-${escapeHtml(session.agent)}" title="${session.agent === "codex" ? "Codex" : "Claude Code"}" aria-label="${session.agent === "codex" ? "Codex" : "Claude Code"}">${agentLabel}</span><span class="row-progress" title="${escapeHtml(t("action-tool-progress"))}"><small>PROG</small><b>${escapeHtml(progress)}</b></span><span class="row-restarts" title="${escapeHtml(t("action-restart-count"))}">↻ ${restartCount}</span></div>
    <div class="row-dwell"><span>${dwell(session.status_since)}</span><small class="row-last-line" title="${escapeHtml(lastLine)}">${escapeHtml(lastLine)}</small></div>
    <div class="row-actions"><button type="button" data-action="pin" class="row-action ${session.pinned ? "row-action-active" : ""}" title="${escapeHtml(pinLabel)}" aria-label="${escapeHtml(pinLabel)} ${escapeHtml(session.name)}">${session.pinned ? "◆" : "◇"}</button><button type="button" data-action="focus" class="row-action" title="${escapeHtml(t("action-focus-terminal"))}" aria-label="${escapeHtml(t("action-focus-session", { name: session.name }))}">↗</button><button type="button" data-action="revive" class="row-action" title="${escapeHtml(t("action-revive", { name: session.name }))}" aria-label="${escapeHtml(t("action-revive", { name: session.name }))}"${reviveHidden}>↻</button><button type="button" data-action="archive" class="row-action" title="${escapeHtml(t("action-archive-stopped"))}" aria-label="${escapeHtml(t("action-archive", { name: session.name }))}"${archiveHidden}>▣</button><button type="button" data-action="kill" class="row-action row-action-danger" title="${escapeHtml(stopLabel)}" aria-label="${escapeHtml(stopLabel)}"${stopHidden}>×</button></div>
    <div class="row-wide-meta"${wideHidden}><span><small>MODEL</small><b data-row-model>${escapeHtml(model)}</b></span><span><small>EFFORT</small><b data-row-effort>${escapeHtml(effort)}</b></span><span><small>COST</small><b data-row-cost>${escapeHtml(cost(session.cost_usd))}</b></span></div>
    <div class="row-reply"${replyHidden}><input data-reply type="text" maxlength="500" placeholder="${escapeHtml(t("action-reply", { name: session.name }))}" aria-label="${escapeHtml(t("action-reply", { name: session.name }))}" /><button type="button" data-action="reply" class="row-reply-send" title="${escapeHtml(t("button-send-reply"))}" aria-label="${escapeHtml(t("action-send-reply", { name: session.name }))}">↵</button></div>
  </article>`;
}

function updateSession(session) {
  const index = state.sessions.findIndex((item) => item.id === session.id);
  const previous = index === -1 ? null : state.sessions[index];
  if (index === -1) state.sessions.push(session);
  else state.sessions[index] = session;
  if (previous) announceStatusChange(session, previous.status);
  renderRows();
  updateTerminalHeader();
}

function removeSession(id) {
  state.sessions = state.sessions.filter((session) => session.id !== id);
  for (const [key, entry] of state.attentionToasts) {
    if (entry.sessionId === id) retractAttentionToast(key);
  }
  if (state.focused === id) {
    state.focused = null;
    resetTerminal("Session exited");
  }
  renderRows();
}

// The xterm element is appended after the placeholder inside an overflow-hidden host,
// so a placeholder left in flow lays the renderer out entirely below the visible box.
function renderTerminalPlaceholder() {
  const attached = Boolean(state.focused);
  $("terminal-placeholder").classList.toggle("view-hidden", attached);
  $("terminal-host").classList.toggle("terminal-host-attached", attached);
  // The host only has a usable box once the placeholder is out of flow.
  if (attached) scheduleFit();
}

function updateTerminalHeader() {
  renderTerminalPlaceholder();
  const session = state.sessions.find((item) => item.id === state.focused);
  if (!session) {
    $("terminal-name").textContent = t("empty-no-focused-session");
    $("terminal-path").textContent = "";
    $("terminal-status").textContent = t("empty-waiting-for-session");
    $("terminal-pulse").className = "terminal-pulse";
    if (state.diagnosticsMode) renderDiagnostics();
    return;
  }
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const label = lifecycleLabel(session);
  $("terminal-name").textContent = session.name;
  $("terminal-path").textContent = session.cwd;
  $("terminal-status").textContent = `${label} · ${dwell(session.status_since)} · ${session.agent === "codex" ? "Codex" : "Claude Code"}`;
  $("terminal-pulse").className = `terminal-pulse pulse-${meta.tone}`;
  if (state.diagnosticsMode) renderDiagnostics();
}

function resetTerminal(status = t("empty-waiting-for-session")) {
  if (state.terminal) state.terminal.reset();
  $("terminal-status").textContent = status;
  updateTerminalHeader();
}

const EXTERNAL_STATE_LABEL = {
  live: { label: "external-running", tone: "sapphire" },
  ended: { label: "external-ended", tone: "overlay0" },
  unknown: { label: "external-unknown", tone: "overlay0" },
};

// Rows for sessions this supervisor did not start. Deliberately actionless: we
// do not own their pty, so offering Stop or Focus would promise something the
// daemon cannot deliver.
function renderExternal() {
  const supervised = new Set(state.sessions.map((session) => String(session.cwd ?? "").toLowerCase()));
  const rows = state.external.filter((session) => session.state !== "ended");
  const view = $("external-view");
  view.classList.toggle("view-hidden", rows.length === 0 && !state.externalError);

  if (state.externalError) {
    $("external-summary").textContent = state.externalError;
    $("external-list").innerHTML = "";
    return;
  }
  const unknown = rows.filter((session) => session.state === "unknown").length;
  $("external-summary").textContent = unknown
    ? `${countMessage("count-external", rows.length)} · ${countMessage("count-unknown-external", unknown)}`
    : countMessage("count-external", rows.length);

  $("external-list").innerHTML = rows
    .map((session) => {
      const meta = EXTERNAL_STATE_LABEL[session.state] ?? EXTERNAL_STATE_LABEL.unknown;
      const label = session.name || folderLabel(session.cwd) || `pid ${session.pid}`;
      const where = session.entrypoint ? `${session.kind ?? "session"} · ${session.entrypoint}` : (session.kind ?? "session");
      const alsoHere = supervised.has(String(session.cwd ?? "").toLowerCase())
        ? '<span class="external-overlap" title="TerminalAI also supervises a session in this folder">same folder</span>'
        : "";
      const externalAriaLabel = `${label}, ${metaLabel(meta)}, ${countMessage("count-external", 1)}`;
      return `<article class="external-row" role="listitem" aria-label="${escapeHtml(externalAriaLabel)}">
        <span class="status-glyph tone-${escapeHtml(meta.tone)}" aria-hidden="true">◦</span>
        <div class="external-identity"><div class="external-name">${escapeHtml(label)}</div><div class="external-meta"><span title="${escapeHtml(String(session.cwd ?? ""))}">${escapeHtml(folderLabel(session.cwd))}</span><span>${escapeHtml(where)}</span>${session.version ? `<span>v${escapeHtml(session.version)}</span>` : ""}</div></div>
        <span class="external-state">${escapeHtml(metaLabel(meta))}</span>
        <span class="external-pid" title="Process id">${escapeHtml(String(session.pid))}</span>
        ${alsoHere}
      </article>`;
    })
    .join("");
}

async function loadExternal() {
  try {
    state.external = (await invoke("external_sessions")) ?? [];
    state.externalError = null;
  } catch (error) {
    // Never render "nothing running" from a failed lookup.
    state.external = [];
    state.externalError = `Could not read external sessions: ${error}`;
  }
  renderExternal();
}

async function loadReview() {
  try {
    const snapshot = await invoke("review_snapshot");
    state.reviews = snapshot.entries ?? [];
    renderReview();
  } catch (error) {
    showToast("Could not read review snapshot: " + error);
  }
}

function setReviewMode(active) {
  if (active && state.preflightMode) setPreflightMode(false);
  state.reviewMode = active;
  syncReviewVisibility();
  if (active) void loadReview();
  else renderRows();
}

/// Land a reviewed session's work, or report exactly why it was refused.
///
/// The target is the operator's own repository root, which is the session's cwd
/// here — a session started in a worktree lands back into that worktree. The
/// daemon re-reads every precondition at land time, so nothing checked here is
/// trusted; the button is only ever a request.
async function landSession(id, cwd, button) {
  const entry = state.reviews.find((review) => review.session_id === id);
  if (!entry) return;
  if (button) {
    button.disabled = true;
    button.textContent = t("review-landing");
  }
  try {
    const outcome = await invoke("land_session", {
      request: {
        source: cwd,
        target: cwd,
        // Pinned to what this review described, so a target that moved while
        // the operator was reading is refused rather than silently landed on.
        expected_target_head: entry.target_head ?? null,
        verify: [],
      },
    });
    if (outcome.outcome === "landed") {
      showToast(t("review-landed", { files: outcome.files_changed }), "success");
      await loadReview();
      return;
    }
    // A refusal names one specific condition. Surfaced whole: a truncated
    // reason is the difference between fixing it and guessing.
    showToast(t("review-land-refused", { reason: refusalText(outcome) }));
  } catch (error) {
    showToast(String(error));
  } finally {
    if (button) {
      button.disabled = false;
      button.textContent = t("review-land");
    }
  }
}

/// Turn a structured refusal into one line the operator can act on.
function refusalText(outcome) {
  switch (outcome.reason) {
    case "target-moved":
      return t("land-target-moved", { expected: outcome.expected, found: outcome.found });
    case "target-dirty":
      return t("land-target-dirty", { paths: (outcome.paths ?? []).join(", ") });
    case "conflict-markers":
      return t("land-conflict-markers", { paths: (outcome.paths ?? []).join(", ") });
    case "patch-did-not-apply":
      return t("land-patch-stale", { detail: outcome.detail });
    case "verify-failed":
      return t("land-verify-failed", { command: outcome.command, output: outcome.output });
    case "verify-failed-and-not-reversed":
      return t("land-verify-not-reversed", {
        command: outcome.command,
        error: outcome.reversal_error,
      });
    case "nothing-to-land":
      return t("land-nothing");
    case "unavailable":
      return t("land-unavailable", { detail: outcome.detail });
    default:
      // A refusal reason this build does not know about must still be shown —
      // swallowing it would render as "nothing happened".
      return String(outcome.reason ?? outcome);
  }
}

async function markReviewed(id, button) {
  if (button) button.disabled = true;
  try {
    await invoke("mark_reviewed", { id });
    const entry = state.reviews.find((review) => review.session_id === id);
    if (entry) entry.reviewed = true;
    renderReview();
    showToast("Session marked reviewed", "success");
  } catch (error) {
    if (button) button.disabled = false;
    showToast("Could not mark session reviewed: " + error);
  }
}

async function loadSnapshot() {
  state.snapshotLoading = true;
  renderSnapshotLoading();
  try {
    const snapshot = await invoke("fleet_snapshot");
    state.sessions = snapshot.sessions ?? [];
    state.focused = snapshot.focused ?? null;
    state.admission = snapshot.admission ?? state.admission;
    const storeQuarantine = snapshot.store_quarantine ?? null;
    if (storeQuarantine !== state.storeQuarantine) state.storeQuarantineDismissed = false;
    state.storeQuarantine = storeQuarantine;
    renderStoreQuarantine();
    renderRows();
    updateTerminalHeader();
    if (state.focused) {
      state.terminal?.reset();
      await attachSessionOutput(state.focused);
      updateTerminalHeader();
      renderRows();
    }
  } catch (error) {
    state.preflightReason = `Daemon unavailable: ${error}`;
    state.preflightMode = true;
    syncPreflightVisibility();
    syncReviewVisibility();
    void loadPreflight(true);
  } finally {
    state.snapshotLoading = false;
    renderSnapshotLoading();
  }
}

async function focusSession(id) {
  const previousFocused = state.focused;
  state.focused = id;
  state.focusGeneration += 1;
  state.terminal?.reset();
  fitTerminal();
  renderRows();
  updateTerminalHeader();
  try {
    await attachSessionOutput(id);
    updateTerminalHeader();
    renderRows();
  } catch (error) {
    state.focused = previousFocused;
    renderRows();
    updateTerminalHeader();
    showToast(`Could not focus session: ${error}`);
  }
}

function createOutputChannel(id) {
  const generation = state.focusGeneration;
  const channel = new Channel();
  channel.onmessage = (data) => writeTerminalBytes(data, id, generation);
  return channel;
}

async function attachSessionOutput(id) {
  const channel = createOutputChannel(id);
  state.outputChannel = channel;
  const session = state.sessions.find((item) => item.id === id);
  if (session && session.status !== "exited") {
    await invoke("attach_session_output", { id, channel });
  } else {
    await invoke("focus_session", { id });
    await invoke("subscribe_output", { id, channel });
    await invoke("stream_scrollback", { id, channel });
  }
  await invoke("mark_read", { id });
}

async function rowAction(action, id, row = null) {
  try {
    if (action === "pin") await invoke("toggle_pin", { id });
    if (action === "focus") await focusSession(id);
    if (action === "reply") {
      const input = row?.querySelector("input[data-reply]");
      const reply = input?.value.trim();
      if (!reply) return;
      const bracketedPaste = `\x1b[200~${reply}\x1b[201~\r`;
      await invoke("write_session", { id, data: bracketedPaste });
      await invoke("mark_read", { id });
      input.value = "";
      showToast("Reply sent", "success");
    }
    if (action === "kill") {
      await invoke("kill_session", { id });
      showToast("Stop signal sent", "success");
    }
    if (action === "revive") {
      await invoke("revive_session", { id });
      showToast("Native session resume started", "success");
    }
    if (action === "archive") {
      await invoke("archive_session", { id });
      showToast("Stopped session archived", "success");
    }
  } catch (error) {
    showToast(String(error));
  }
}

function defaultSpec() {
  return {
    agent: "claude",
    name: null,
    cwd: "",
    model: null,
    effort: null,
    permission: "ask",
    sandbox: null,
    profile: null,
    add_dirs: [],
    resume: { kind: "new" },
    max_budget_usd: null,
    web_search: false,
    initial_prompt: null,
    extra_args: [],
    environment: { setup: null, teardown: null, port_base: 42000, port_count: 4 },
  };
}

function readSpec() {
  const agent = $("agent-input").value;
  const resumeKind = $("resume-input").value;
  const nativeId = $("resume-id-input").value.trim();
  const resume = resumeKind === "session" ? { kind: "session", id: nativeId } : resumeKind === "fork" ? { kind: "fork", id: nativeId } : { kind: resumeKind };
  const budget = $("budget-input").value.trim();
  const portBase = Number.parseInt($("port-base-input").value, 10);
  const portCount = Number.parseInt($("port-count-input").value, 10);
  return {
    agent,
    name: $("name-input").value.trim() || null,
    cwd: $("cwd-input").value.trim(),
    model: $("model-input").value.trim() || null,
    effort: $("effort-input").value.trim() || null,
    permission: $("permission-input").value,
    sandbox: agent === "codex" ? $("sandbox-input").value : null,
    profile: agent === "codex" ? $("profile-input").value.trim() || null : null,
    add_dirs: [...state.extraDirs],
    resume,
    max_budget_usd: agent === "claude" && budget ? Number(budget) : null,
    web_search: agent === "codex" && $("search-input").checked,
    initial_prompt: $("prompt-input").value.trim() || null,
    extra_args: [],
    environment: {
      setup: $("setup-hook-input").value.trim() || null,
      teardown: $("teardown-hook-input").value.trim() || null,
      port_base: Number.isInteger(portBase) ? portBase : 42000,
      port_count: Number.isInteger(portCount) ? portCount : 4,
    },
  };
}

function writeSpec(spec) {
  $("agent-input").value = spec.agent ?? "claude";
  $("name-input").value = spec.name ?? "";
  $("cwd-input").value = spec.cwd ?? "";
  $("model-input").value = spec.model ?? "";
  $("effort-input").value = spec.effort ?? "";
  $("permission-input").value = spec.permission ?? "ask";
  $("sandbox-input").value = spec.sandbox ?? "workspace-write";
  $("profile-input").value = spec.profile ?? "";
  $("resume-input").value = spec.resume?.kind ?? "new";
  $("resume-id-input").value = spec.resume?.id ?? "";
  $("budget-input").value = spec.max_budget_usd ?? "";
  $("search-input").checked = Boolean(spec.web_search);
  $("port-base-input").value = spec.environment?.port_base ?? 42000;
  $("port-count-input").value = spec.environment?.port_count ?? 4;
  $("setup-hook-input").value = spec.environment?.setup ?? "";
  $("teardown-hook-input").value = spec.environment?.teardown ?? "";
  $("prompt-input").value = spec.initial_prompt ?? "";
  state.extraDirs = spec.add_dirs ?? [];
  $("extra-dirs-input").value = state.extraDirs.join("; ");
  syncAgentFields();
  schedulePreview();
}

function capabilityForAgent(agent = $("agent-input").value) {
  return state.capabilities[agent] ?? null;
}

function renderCapabilityFields() {
  const capabilities = capabilityForAgent();
  const selectedModel = $("model-input").value.trim();
  const models = Array.isArray(capabilities?.models) ? capabilities.models : [];
  $("model-suggestions").innerHTML = models
    .filter((model) => !model.hidden)
    .map((model) => `<option value="${escapeHtml(model.id)}"></option>`)
    .join("");
  const selected = models.find((model) => model.id === selectedModel);
  const efforts = selected?.supported_efforts?.length
    ? selected.supported_efforts
    : (capabilities?.efforts ?? []);
  $("effort-suggestions").innerHTML = efforts
    .map((effort) => `<option value="${escapeHtml(effort)}"></option>`)
    .join("");
  const warnings = [];
  if (capabilities?.warning) warnings.push(capabilities.warning);
  if (selectedModel && models.length && !models.some((model) => model.id === selectedModel)) {
    warnings.push(`Model ${selectedModel} is not in the detected catalog; it will be passed through.`);
  }
  const selectedEffort = $("effort-input").value.trim();
  if (selectedEffort && efforts.length && !efforts.includes(selectedEffort)) {
    warnings.push(`Reasoning effort ${selectedEffort} is not advertised for this model; it will be passed through.`);
  }
  const note = $("capability-note");
  note.classList.toggle("field-hidden", warnings.length === 0);
  note.textContent = warnings.join(" ");
}

async function loadAgentCapabilities(agent = $("agent-input").value) {
  const request = ++state.capabilityRequest;
  renderCapabilityFields();
  try {
    const capabilities = await invoke("agent_capabilities", { agent, configuredPath: null });
    if (request !== state.capabilityRequest) return;
    state.capabilities[agent] = capabilities;
  } catch (error) {
    if (request !== state.capabilityRequest) return;
    state.capabilities[agent] = {
      models: [],
      efforts: [],
      warning: `Runtime capability probe unavailable: ${String(error)} Custom values remain allowed.`,
    };
  }
  renderCapabilityFields();
}

function syncAgentFields() {
  const codex = $("agent-input").value === "codex";
  document.querySelectorAll(".codex-only").forEach((element) => element.classList.toggle("field-hidden", !codex));
  document.querySelectorAll(".claude-only").forEach((element) => element.classList.toggle("field-hidden", codex));
  renderCapabilityFields();
  if (!codex && $("permission-input").value === "plan") $("permission-input").value = "ask";
  document.querySelectorAll(".resume-id-field").forEach((element) => element.classList.toggle("field-hidden", $("resume-input").value === "new" || $("resume-input").value === "last"));
}

function schedulePreview() {
  clearTimeout(state.previewTimer);
  state.previewTimer = setTimeout(updatePreview, 180);
}

async function updatePreview() {
  const spec = readSpec();
  if (!spec.cwd) {
    $("preview-output").textContent = "Choose a project folder to preview the exact command vector.";
    $("preview-state").textContent = "Waiting for a valid folder";
    return;
  }
  $("preview-state").textContent = "Resolving native binary…";
  try {
    const command = await invoke("preview_launch", invokeArgs(spec));
    $("preview-output").textContent = command;
    $("preview-state").textContent = "Exact argv preview";
  } catch (error) {
    $("preview-output").textContent = String(error);
    $("preview-state").textContent = "Launch refused";
  }
}

async function launchCurrentSpec() {
  const spec = readSpec();
  if (!spec.cwd) {
    showToast("Choose a project folder first");
    return false;
  }
  try {
    const receipt = await invoke("launch_session", invokeArgs(spec));
    $("launcher-dialog").close();
    const agentLabel = spec.agent === "codex" ? "Codex" : "Claude Code";
    showToast(
      receipt?.queued
        ? agentLabel + " session queued for an admission slot"
        : agentLabel + " session launched",
      "success",
    );
    return true;
  } catch (error) {
    showToast(String(error));
    return false;
  }
}

async function loadPresets() {
  try {
    state.presets = await invoke("list_presets");
    $("preset-select").innerHTML = `<option value="">Presets</option>${state.presets.map((preset) => `<option value="${escapeHtml(preset.name)}">${escapeHtml(preset.name)}</option>`).join("")}`;
  } catch (error) {
    showToast(`Could not load presets: ${error}`);
  }
}

async function saveCurrentPreset() {
  const name = $("preset-name-input").value.trim();
  if (!name) {
    showToast("Enter a preset name first");
    $("preset-name-input").focus();
    return;
  }
  try {
    await invoke("save_preset", { preset: { name, spec: readSpec(), configured_path: null } });
    await loadPresets();
    $("preset-name-input").value = "";
    showToast(`Preset “${name}” saved`, "success");
  } catch (error) {
    showToast(String(error));
  }
}

function loadSelectedPreset() {
  const preset = state.presets.find((entry) => entry.name === $("preset-select").value);
  if (!preset) return;
  writeSpec(preset.spec);
  $("launcher-dialog").showModal();
  void loadAgentCapabilities($("agent-input").value);
}

function openLauncher() {
  writeSpec(defaultSpec());
  $("launcher-dialog").showModal();
  $("cwd-input").focus();
  void loadAgentCapabilities($("agent-input").value);
}

const DEFAULT_COLS = 120;
const DEFAULT_ROWS = 40;
/// Agent TUIs hard-wrap and do not reflow, so a resize arriving mid-drag
/// corrupts the very output the supervisor parses for status. Coalesce.
const RESIZE_DEBOUNCE_MS = 180;

function terminalSizeLabel(cols, rows) {
  $("terminal-grid").textContent = `GRID  ${cols} × ${rows}`;
}

/// Fit the grid to the pane and tell the pty, at most once per settled resize.
function fitTerminal({ notify = true } = {}) {
  if (!state.terminal || !state.fitAddon) return;
  const host = $("terminal-host");
  if (!host || host.clientWidth <= 0 || host.clientHeight <= 0) return;
  let size;
  try {
    size = state.fitAddon.proposeDimensions();
  } catch {
    return;
  }
  if (!size || !Number.isFinite(size.cols) || !Number.isFinite(size.rows)) return;
  const cols = Math.max(20, Math.floor(size.cols));
  const rows = Math.max(5, Math.floor(size.rows));
  if (state.terminal.cols !== cols || state.terminal.rows !== rows) {
    state.terminal.resize(cols, rows);
  }
  terminalSizeLabel(cols, rows);
  if (!notify || !state.focused) return;
  const signature = `${cols}x${rows}`;
  if (state.lastSentSize === signature) return;
  state.lastSentSize = signature;
  invoke("resize_session", { id: state.focused, rows, cols }).catch(() => {
    // A resize the daemon refuses is not worth a toast; the next one retries.
    state.lastSentSize = null;
  });
}

function scheduleFit() {
  if (state.resizeTimer) clearTimeout(state.resizeTimer);
  state.resizeTimer = setTimeout(() => {
    state.resizeTimer = null;
    fitTerminal();
  }, RESIZE_DEBOUNCE_MS);
}

function observeTerminalSize() {
  const host = $("terminal-host");
  if (!host) return;
  if (typeof ResizeObserver === "function") {
    new ResizeObserver(scheduleFit).observe(host);
  }
  window.addEventListener("resize", scheduleFit);
}

/// Open an OSC 8 hyperlink a session emitted.
///
/// The scheme allowlist lives in Rust, not here: this is agent-controlled text,
/// and the renderer is the wrong place to be the only thing standing between it
/// and `ShellExecute`. A refusal is shown, never swallowed.
async function openSessionLink(uri) {
  try {
    const opened = await invoke("open_external_url", { url: uri });
    showToast(`Opened ${new URL(opened).host || opened}`, "success");
  } catch (error) {
    showToast(String(error));
  }
}

/// Swap the DOM renderer for the WebGL one.
///
/// Kept separate from `setupTerminal` because every step here can legitimately
/// fail on a machine with no usable GPU path, and the terminal must still work
/// when it does. WebView2 falls back to SwiftShader in some configurations, and
/// the context can also be lost later — after a driver reset or a GPU process
/// crash — so `onContextLoss` disposes the addon and returns the terminal to the
/// DOM renderer rather than leaving a blank pane.
function useWebglRenderer(terminal) {
  let addon;
  try {
    addon = new WebglAddon();
  } catch (error) {
    console.info("WebGL renderer unavailable, using the DOM renderer", error);
    return null;
  }
  addon.onContextLoss(() => {
    console.info("WebGL context lost, falling back to the DOM renderer");
    addon.dispose();
    state.webglAddon = null;
  });
  try {
    terminal.loadAddon(addon);
  } catch (error) {
    // loadAddon is where context creation actually happens.
    console.info("WebGL context could not be created, using the DOM renderer", error);
    addon.dispose();
    return null;
  }
  return addon;
}

function setupTerminal() {
  state.terminal = new Terminal({
    // Required by the unicode11 addon. Without it xterm measures character
    // widths against Unicode 6, while the Rust grid uses `unicode-width`
    // against a modern table — the two then disagree about where a line wraps,
    // and the row status inferred from the Rust grid stops matching the pane.
    allowProposedApi: true,
    // OSC 8 hyperlinks reach the pane already, because the focused renderer
    // replays raw PTY bytes. Without a handler xterm underlines them and
    // clicking does nothing.
    linkHandler: {
      activate: (event, uri) => {
        event.preventDefault();
        openSessionLink(uri);
      },
    },
    convertEol: false,
    cursorBlink: true,
    cursorStyle: "bar",
    fontFamily: "'Cascadia Code', 'SFMono-Regular', Consolas, monospace",
    fontSize: 13,
    lineHeight: 1.25,
    scrollback: 2000,
    screenReaderMode: false,
    theme: {
      background: "#11111b",
      foreground: "#cdd6f4",
      cursor: "#f5e0e6",
      selectionBackground: "#585b70",
      black: "#181825",
      red: "#f38ba8",
      green: "#a6e3a1",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#cba6f7",
      cyan: "#94e2d5",
      white: "#bac2de",
    },
  });
  state.fitAddon = new FitAddon();
  state.terminal.loadAddon(state.fitAddon);
  const unicode11 = new Unicode11Addon();
  state.terminal.loadAddon(unicode11);
  state.terminal.unicode.activeVersion = "11";
  state.terminal.open($("terminal-host"));
  // Must follow `open`: the WebGL addon needs an attached element to create a
  // context against.
  state.webglAddon = useWebglRenderer(state.terminal);
  state.terminal.resize(DEFAULT_COLS, DEFAULT_ROWS);
  // The addon was constructed and registered but never called, so the grid
  // stayed at its hard-coded size no matter how large the pane was.
  observeTerminalSize();
  state.terminal.onData(async (data) => {
    if (!state.focused) return;
    try {
      await invoke("write_session", { id: state.focused, data });
    } catch (error) {
      showToast(String(error));
    }
  });
}

async function handleDaemonEvent(event) {
  switch (event.kind) {
    case "session-updated":
      updateSession(event.session);
      break;
    case "session-removed":
      removeSession(event.id);
      break;
    case "notification":
      if (event.event?.kind === "raised") showAttentionToast(event.event.notification);
      if (event.event?.kind === "retracted") retractAttentionToast(event.event.dedup_key);
      break;
    case "output":
      writeTerminalBytes(event.data, event.id);
      break;
    default:
      break;
  }
}

function bindEvents() {
  $("new-session-button").addEventListener("click", openLauncher);
  $("empty-new-button").addEventListener("click", openLauncher);
  $("refresh-button").addEventListener("click", () => {
    void loadSnapshot();
    void loadExternal();
  });
  $("dismiss-store-quarantine").addEventListener("click", () => {
    state.storeQuarantineDismissed = true;
    renderStoreQuarantine();
  });
  $("preflight-toggle").addEventListener("click", () => {
    if (state.preflightMode) setPreflightMode(false);
    else {
      setPreflightMode(true);
      void loadPreflight(true);
    }
  });
  $("preflight-recheck").addEventListener("click", () => void loadPreflight(true));
  $("preflight-close").addEventListener("click", () => setPreflightMode(false));
  $("preflight-list").addEventListener("click", (event) => {
    const button = event.target.closest("button[data-preflight-action]");
    if (!button) return;
    void handlePreflightAction(button.dataset.preflightAction, button.dataset.preflightId, button);
  });
  $("review-toggle").addEventListener("click", () => setReviewMode(!state.reviewMode));
  $("review-refresh").addEventListener("click", loadReview);
  $("review-list").addEventListener("click", (event) => {
    const button = event.target.closest("button[data-review-action]");
    if (!button) return;
    if (button.dataset.reviewAction === "mark-reviewed") {
      void markReviewed(button.dataset.reviewId, button);
    }
    if (button.dataset.reviewAction === "land") {
      void landSession(button.dataset.reviewId, button.dataset.reviewCwd, button);
    }
  });
  $("diagnostics-toggle").addEventListener("click", () => setDiagnosticsMode(!state.diagnosticsMode));
  $("logs-toggle").addEventListener("click", () => setLogsMode(!state.logsMode));
  $("screen-reader-toggle").addEventListener("click", () => setScreenReaderMode(!state.screenReaderMode));
  setScreenReaderMode(state.screenReaderMode);
  $("diagnostics-host").addEventListener("click", (event) => {
    if (!event.target.closest("button[data-diagnostics-action=preflight]")) return;
    setPreflightMode(true);
    void loadPreflight(true);
  });
  $("update-check-button").addEventListener("click", () => void checkForUpdates());
  $("filter-input").addEventListener("input", renderRows);
  $("fleet-list").addEventListener("mouseenter", beginFleetOrderFreeze);
  $("fleet-list").addEventListener("mouseleave", releaseFleetOrderFreeze);
  $("fleet-list").addEventListener("focusin", beginFleetOrderFreeze);
  $("fleet-list").addEventListener("focusout", releaseFleetOrderFreeze);
  $("apply-fleet-order").addEventListener("click", applyFleetOrder);
  $("wide-toggle").addEventListener("click", () => {
    state.wideMode = !state.wideMode;
    renderRows();
  });
  $("attention-filter").addEventListener("click", () => {
    state.attentionOnly = !state.attentionOnly;
    $("attention-filter").setAttribute("aria-pressed", String(state.attentionOnly));
    $("attention-filter").classList.toggle("attention-filter-active", state.attentionOnly);
    renderRows();
  });
  document.addEventListener("keydown", (event) => {
    const tag = event.target?.tagName?.toLowerCase();
    if (event.key === "/" && !event.target?.isContentEditable && !["input", "textarea", "select"].includes(tag)) {
      event.preventDefault();
      $("filter-input").focus({ preventScroll: true });
      $("filter-input").select();
    }
  });
  $("agent-input").addEventListener("change", () => {
    syncAgentFields();
    void loadAgentCapabilities($("agent-input").value);
    schedulePreview();
  });
  ["cwd-input", "model-input", "name-input", "effort-input", "permission-input", "sandbox-input", "profile-input", "resume-input", "resume-id-input", "budget-input", "port-base-input", "port-count-input", "setup-hook-input", "teardown-hook-input", "prompt-input", "search-input"].forEach((id) => {
    $(id).addEventListener("input", () => {
      if (id === "resume-input" || id === "model-input" || id === "effort-input") syncAgentFields();
      schedulePreview();
    });
    $(id).addEventListener("change", () => {
      if (id === "resume-input" || id === "model-input" || id === "effort-input") syncAgentFields();
      schedulePreview();
    });
  });
  $("pick-folder-button").addEventListener("click", async () => {
    const folder = await invoke("pick_folder");
    if (folder) {
      $("cwd-input").value = folder;
      schedulePreview();
    }
  });
  $("pick-extra-button").addEventListener("click", async () => {
    const folders = await invoke("pick_extra_dirs");
    if (folders?.length) {
      state.extraDirs = folders;
      $("extra-dirs-input").value = folders.join("; ");
      schedulePreview();
    }
  });
  $("save-preset-button").addEventListener("click", saveCurrentPreset);
  $("launch-preset-button").addEventListener("click", loadSelectedPreset);
  $("cancel-launch-button").addEventListener("click", () => $("launcher-dialog").close());
  $("close-launcher-button").addEventListener("click", () => $("launcher-dialog").close());
  // Launching costs tokens and writes to a real repository, so it is reachable only
  // from the launch button. The form never submits: implicit submission on Enter in
  // any field would otherwise spawn an agent the operator never asked for.
  $("launcher-form").addEventListener("submit", (event) => event.preventDefault());
  $("launch-button").addEventListener("click", () => void launchCurrentSpec());
  $("terminal-clear").addEventListener("click", () => state.terminal?.clear());
  $("terminal-resize").addEventListener("click", async () => {
    if (!state.focused) return;
    try {
      state.lastSentSize = null;
      fitTerminal();
      showToast(`Terminal refitted to ${state.terminal.cols} × ${state.terminal.rows}`, "success");
    } catch (error) {
      showToast(String(error));
    }
  });
}

async function start() {
  localizeDom();
  setupTerminal();
  startPinnedPolling();
  bindEvents();
  syncAgentFields();
  try {
    await listen("terminalai:event", ({ payload }) => handleDaemonEvent(payload));
    await listen("terminalai:logs", ({ payload }) => appendLogs(payload));
    // A clicked Windows toast names the session that wanted attention. Focusing
    // it is the whole point of the click — landing on the fleet list and making
    // the operator find the row again would waste the notification.
    await listen("terminalai:focus-session", ({ payload }) => {
      const id = typeof payload === "string" ? payload : payload?.id;
      if (id) void focusSession(id);
    });
  } catch (error) {
    showToast(`Event stream unavailable: ${error}`);
  }
  await loadPreflight();
  await Promise.all([loadSnapshot(), loadPresets(), loadExternal()]);
  setInterval(() => {
    renderRows();
    updateTerminalHeader();
  }, 1000);
}

async function startWhenReady() {
  if (WDIO_BUILD) {
    await new Promise((resolve) => {
      window.addEventListener("terminalai-wdio-ready", resolve, { once: true });
    });
  }
  await start();
}

void startWhenReady();
