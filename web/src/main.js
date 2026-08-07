import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { contextLabel, contextTitle, contextTone } from "./contextPressure.js";
import { renderSearchResults, searchSummary } from "./fleetSearch.js";
import { pendingApprovals, renderApprovals, requestLine, waitingSince } from "./approvals.js";
import { renderRestoreOutcomes, renderWorkingSet, summarizeRestore } from "./workingSets.js";
import { reconcileGroupChip, reconcileKeyedRows } from "./fleetRows.js";
import { countMessage, localizeDom, relativeDwell, t } from "./i18n.js";
import { quotaLabel, quotaUnreportedLabel, rateLimitTitle, rateLimitedLabel } from "./rateLimit.js";
import { spendCeiling, spendCeilingTitle } from "./spendCeiling.js";
import {
  coverage,
  fleetTotals,
  folderOf,
  formatCost,
  formatTokens,
  pricingFreshness,
  PRICING_STALE_AFTER_DAYS,
  rollupBy,
  TOKEN_FIELDS,
} from "./rollup.js";
import { defaultSelection, isEligible, summarize, targets } from "./broadcast.js";
import { hasOpenWork, openItemsCell, sortProjects, stalenessLabel, summarize as summarizeProjects } from "./projects.js";
import { renderSessionHistory } from "./sessionHistory.js";
import { renderWorktrees } from "./worktrees.js";
import { wireOverflowMenus } from "./menus.js";
import { checkForUpdates as runUpdateCheck, RELEASES_PAGE } from "./updateCheck.js";
import { systemTimeMs } from "./time.js";
import "./styles.css";
import { createRowRenderer } from "./rowMarkup.js";

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
  blocked: { glyph: "⊘", label: "preflight-blocked", tone: "red" },
  unsupported: { glyph: "—", label: "preflight-not-applicable", tone: "overlay0" },
};
const FALLBACK_APP_VERSION = __APP_VERSION__;

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

/// How long a working session may dwell before the supervisor calls it stalled.
/// Mirrors `STALL_THRESHOLD` in `crates/terminalai-core/src/session.rs`; the
/// flag itself is computed there, and this is only used to say the threshold out
/// loud so the mark is not an unexplained badge.
const STALL_THRESHOLD_MINUTES = 15;

/// How long a session may say nothing at all before the supervisor calls it
/// unresponsive: `PROGRESS_DEADLINE` x `PROGRESS_FAILURE_THRESHOLD` in
/// `session.rs`. A different measure from the one above, and a stronger one —
/// that asks how long a status has been held, this asks whether the process is
/// still producing anything.
const SILENCE_THRESHOLD_MINUTES = 15;

/// The supervisor found the process alive and completely silent. Stronger
/// evidence than the dwell-based stall flag, so it wins the label.
function isUnresponsive(session) {
  return session?.health === "unresponsive";
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

function restartCount(session) {
  const restarts = Number(session?.restarts);
  return Number.isInteger(restarts) ? restarts : 0;
}

/// Why a session ended, when "it ended" is not the whole story.
///
/// Only the terminal phases carry one: everything else already says what it is
/// in its own label.
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

/// The colour a row's glyph takes.
///
/// Phase overrides status here, and only for the two terminal phases: an exited
/// session is grey, but one the supervisor gave up on is a failure and must not
/// look like a job that completed.
function lifecycleTone(session, meta) {
  if (session?.phase === "failed") return "red";
  // Louder than the yellow a healthy working row gets: the whole point is that
  // it no longer looks like one.
  if (isUnresponsive(session) || session?.stalled) return "peach";
  if (session?.phase === "finished") return "green";
  return meta.tone;
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
  storeWriteError: null,
  terminal: null,
  outputChannel: null,
  fitAddon: null,
  webglAddon: null,
  /// Pinned panes render from Rust-side grid snapshots, polled on a timer. They
  /// deliberately do not get xterm instances: one renderer is what lets the
  /// fleet hold ~29 rows, and three more would undo that.
  pinnedTimer: null,
  pinnedGrids: new Map(),
  /// Structured filters, distinct from the free-text box: text matches anything
  /// on a row, while these are exact dimensions an operator thinks in.
  agentFilter: "all",
  statusFilter: "all",
  /// Grouping reorders the list so members of a group are adjacent and labels
  /// each row with its group. Headers are deliberately not inserted: the list is
  /// an ARIA listbox, and a non-option child would break its semantics.
  groupBy: "none",
  focusGeneration: 0,
  focusQueue: Promise.resolve(),
  resizeTimer: null,
  lastSentSize: null,
  previewTimer: null,
  previewRequest: 0,
  preflight: null,
  preflightMode: false,
  preflightLoading: false,
  preflightReason: null,
  capabilities: {},
  capabilityRequest: 0,
  snapshotLoading: true,
  snapshotQueue: Promise.resolve(),
  snapshotEvents: [],
  historyLoading: false,
  broadcastSelection: [],
  templates: [],
  projects: [],
  scannedProjects: [],
  projectsError: null,
  projectRoots: [],
  projectRootsError: null,
  queueSession: null,
  queuePrompts: [],
  queueError: null,
  storedPrompts: [],
  storedPromptsError: null,
  activeStoredPrompt: null,
  workRun: null,
  reviewError: null,
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

function renderDataError(container, message, action, retry) {
  container.innerHTML = `<div class="data-error" role="alert"><p>${escapeHtml(message)}</p><button type="button" class="button button-secondary" data-retry-action="${escapeHtml(action)}">${escapeHtml(t("button-retry"))}</button></div>`;
  const button = container.querySelector("[data-retry-action]");
  if (button?.dataset.retryAction === action) button.addEventListener("click", () => void retry());
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

/// The fleet's state is not reaching disk.
///
/// Deliberately not dismissable, unlike the quarantine banner beside it. A
/// quarantine is a past event the operator acknowledges once; this is an
/// ongoing condition that clears itself the moment a write succeeds, so
/// dismissing it would hide a live problem rather than an old one.
function renderStoreWriteError() {
  const banner = $("store-write-banner");
  const error = state.storeWriteError;
  banner.classList.toggle("view-hidden", !error);
  $("store-write-message").textContent = error
    ? t("store-write-failed-detail", { error })
    : "";
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

/// One banner for the whole fleet, not one failure per queued entry.
///
/// Driven only by an explicit expiry. A probe that could not run reports
/// `unknown` and is deliberately absent from this list, because a banner the
/// operator cannot clear by signing in is worse than no banner.
function renderAuthBanner() {
  const banner = $("auth-banner");
  const expired = Array.isArray(state.admission.expired_auth) ? state.admission.expired_auth : [];
  banner.classList.toggle("view-hidden", expired.length === 0);
  if (!expired.length) return;
  const agents = expired
    .map((entry) => (entry.agent === "codex" ? "Codex" : "Claude Code"))
    .join(", ");
  $("auth-banner-message").textContent = t("auth-expired-detail", { agents });
}

/// Read a settings field as an optional number.
///
/// Empty means "no limit", which is a different fact from zero: zero would ask
/// the daemon for a ceiling of nothing, and the daemon treats that as disabled
/// anyway, so the two would silently agree by accident rather than by intent.
function optionalNumber(id) {
  const raw = $(id).value.trim();
  if (!raw) return null;
  const value = Number(raw);
  return Number.isFinite(value) && value >= 0 ? value : null;
}

async function openSettings() {
  try {
    const settings = await invoke("admission_config");
    $("settings-max-live").value = settings.max_live_sessions ?? "";
    $("settings-default-budget").value = settings.default_budget_usd ?? "";
    $("settings-spend-ceiling").value = settings.spend_ceiling_usd ?? "";
    $("settings-spend-window").value = settings.spend_window_hours
      ? Math.round(settings.spend_window_hours)
      : "";
    $("settings-memory-budget").value = settings.memory_budget_mb ?? "";
    $("settings-memory-cap").value = settings.session_memory_cap_mb ?? "";
    $("settings-max-processes").value = settings.max_processes_per_session ?? "";
    const fromEnvironment = Array.isArray(settings.from_environment) ? settings.from_environment : [];
    const note = $("settings-environment-note");
    note.hidden = fromEnvironment.length === 0;
    note.textContent = fromEnvironment.length
      ? t("settings-from-environment", { names: fromEnvironment.join(", ") })
      : "";
    $("settings-error").hidden = true;
    $("settings-dialog").showModal();
  } catch (error) {
    showToast(String(error));
  }
}

async function saveSettings() {
  const maxLive = Number($("settings-max-live").value.trim());
  if (!Number.isInteger(maxLive) || maxLive < 1) {
    const problem = $("settings-error");
    problem.textContent = t("settings-max-live") + ": 1+";
    problem.hidden = false;
    return;
  }
  const settings = {
    max_live_sessions: maxLive,
    default_budget_usd: optionalNumber("settings-default-budget"),
    spend_ceiling_usd: optionalNumber("settings-spend-ceiling"),
    spend_window_hours: optionalNumber("settings-spend-window") || 24,
    memory_budget_mb: optionalNumber("settings-memory-budget"),
    session_memory_cap_mb: optionalNumber("settings-memory-cap"),
    max_processes_per_session: optionalNumber("settings-max-processes"),
  };
  try {
    await invoke("set_admission", { settings });
    $("settings-dialog").close();
    showToast(t("settings-saved"), "success");
    await loadSnapshot();
  } catch (error) {
    const problem = $("settings-error");
    problem.textContent = String(error);
    problem.hidden = false;
  }
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
  return value === "" ? "—" : formatCost(value);
}

function reviewNumber(value) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.floor(number) : 0;
}

/// The one result the operator can act on stays where the action is.
///
/// A newer version is the only outcome of the check that asks for anything, and
/// it used to arrive as a toast: gone in four seconds, no link, and no way back
/// to it except running the check again — while the message itself said to go to
/// GitHub. The other outcomes need nothing, so they stay toasts.
function showUpdateResult(message) {
  const result = $("update-result");
  result.classList.toggle("view-hidden", !message);
  $("update-result-message").textContent = message ?? "";
}

/// Bind the update check to this module's collaborators.
///
/// The check itself lives in `updateCheck.js` and takes what it needs, so its
/// version comparison is exercisable without a DOM, a backend or the network.
function checkForUpdates() {
  return runUpdateCheck({
    $,
    t,
    invoke,
    state,
    showToast,
    showUpdateResult,
    fallbackVersion: FALLBACK_APP_VERSION,
  });
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
  return Number.isFinite(time) ? new Date(time).toISOString().replace(".000Z", "Z").replace("T", " ") : t("unknown-time");
}

function renderDiagnostics() {
  const host = $("diagnostics-host");
  const session = state.sessions.find((item) => item.id === state.focused);
  if (!session) {
    const structure = "empty";
    const message = t("empty-focus-diagnostics");
    const empty = host.querySelector(".diagnostics-empty");
    if (host.dataset.diagnosticsStructure !== structure || !empty) {
      host.innerHTML = '<div class="diagnostics-empty">' + escapeHtml(message) + "</div>";
      host.dataset.diagnosticsStructure = structure;
    } else if (empty.textContent !== message) {
      empty.textContent = message;
    }
    return;
  }

  const history = Array.isArray(session.status_history) ? [...session.status_history].reverse() : [];
  const structure = JSON.stringify([session.id, history]);
  const latest = history[0];
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const label = lifecycleLabel(session);
  const source = latest?.source ? diagnosticSource(latest.source) : t("diagnostics-unavailable");

  // Status dwell changes every second, but the timeline does not. Update only
  // the small live fields so selecting a reason never loses its DOM node.
  if (host.dataset.diagnosticsStructure === structure) {
    const heading = host.querySelector(".diagnostics-heading");
    const headingName = heading?.querySelector("h2");
    const headingPath = heading?.querySelector("p");
    const glyph = heading?.querySelector(".status-glyph");
    const current = host.querySelector(".diagnostics-current");
    const currentStatus = current?.querySelector("b");
    const currentDetail = current?.querySelector("span:last-child");
    if (headingName && headingPath && glyph && currentStatus && currentDetail) {
      headingName.textContent = session.name;
      headingPath.textContent = session.cwd;
      glyph.className = "status-glyph tone-" + lifecycleTone(session, meta);
      glyph.title = label;
      glyph.textContent = meta.glyph;
      currentStatus.textContent = label;
      // The terminal phases carry a reason; everything else says it in its label.
      const detail = lifecycleDetail(session);
      currentDetail.textContent = (detail ? detail + " · " : "")
        + t("diagnostics-for", { dwell: dwell(session.status_since) }) + " · " + t("diagnostics-source", { source });
      return;
    }
  }

  const timeline = history.length
    ? history.map((entry) => {
      const entryMeta = STATUS_META[entry.to] ?? STATUS_META.exited;
      const from = entry.from ? statusLabel(entry.from) : t("session-created");
      const reason = formatReason(entry.reason, entry.detail);
      return '<li class="diagnostic-event"><span class="diagnostic-event-glyph tone-' + entryMeta.tone + '" aria-hidden="true">' + entryMeta.glyph + '</span><div class="diagnostic-event-body"><div><b>' + escapeHtml(metaLabel(entryMeta)) + '</b><span>' + escapeHtml(t("diagnostics-from", { status: from })) + '</span></div><small>' + escapeHtml(diagnosticSource(entry.source)) + ' · ' + escapeHtml(diagnosticTime(entry.at)) + '</small>' + (reason ? '<p>' + escapeHtml(reason) + '</p>' : '') + '</div></li>';
    }).join("")
    : '<li class="diagnostics-empty">' + escapeHtml(t("empty-no-transition-history")) + "</li>";
  host.innerHTML =
    '<div class="diagnostics-heading"><div><span class="eyebrow">' + escapeHtml(t("diagnostics-why-this-state")) + '</span><h2>' + escapeHtml(session.name) + '</h2><p>' + escapeHtml(session.cwd) + '</p></div><div class="diagnostics-heading-actions"><button type="button" class="button button-quiet" data-diagnostics-action="preflight">' + escapeHtml(t("button-preflight")) + '</button><span class="status-glyph tone-' + lifecycleTone(session, meta) + '" title="' + escapeHtml(label) + '" aria-hidden="true">' + meta.glyph + '</span></div></div>' +
    '<div class="diagnostics-current"><span>' + escapeHtml(t("diagnostics-current-status")) + '</span><b>' + escapeHtml(label) + '</b><span>' + escapeHtml((lifecycleDetail(session) ? lifecycleDetail(session) + ' · ' : '') + t("diagnostics-for", { dwell: dwell(session.status_since) }) + ' · ' + t("diagnostics-source", { source })) + '</span></div>' +
    '<ol class="diagnostics-timeline">' + timeline + "</ol>";
  host.dataset.diagnosticsStructure = structure;
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
  return Number.isFinite(time) ? new Date(time).toISOString().replace(".000Z", "Z").replace("T", " ") : t("unknown-time");
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
    state.preflightReason = t("preflight-run-error", { error: String(error) });
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
      showToast(t("preflight-fix-applied", { id }), "success");
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
    state.preflightReason = t("preflight-action-error", { action, id, error: String(error) });
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
  if (state.reviewError) {
    $("review-summary").textContent = t("review-unavailable");
    $("review-empty").classList.add("view-hidden");
    renderDataError(
      $("review-list"),
      t("review-load-error", { error: state.reviewError }),
      "review",
      loadReview,
    );
    return;
  }
  const entries = Array.isArray(state.reviews) ? state.reviews : [];
  const pending = entries.filter((entry) => !entry.reviewed).length;
  const conflicts = entries.filter((entry) => (entry.conflicts?.length ?? 0) > 0 || reviewNumber(entry.conflict_markers) > 0).length;
  const timedOut = entries.filter((entry) => entry.timed_out === true).length;
  $("review-summary").textContent = `${t("sessions-count", { count: entries.length })} · ${countMessage("count-pending", pending)} · ${countMessage("count-conflict", conflicts)}${timedOut ? ` · ${countMessage("count-timed-out", timedOut)}` : ""}`;
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
  const reviewError = entry.error || entry.land_error;
  const errorMarkup = reviewError ? '<div class="review-error" role="alert">' + escapeHtml(reviewError) + "</div>" : "";
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

/// A session's private commit, or an em dash when it has not been sampled.
///
/// Never zero from an absent reading: a session using nothing and a session we
/// could not measure are different facts, and the row has to keep saying which.
function memory(bytes) {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value <= 0) return "—";
  if (value >= 1024 * 1024 * 1024) return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  return `${Math.round(value / (1024 * 1024))} MB`;
}

function renderSummary() {
  // Matches the daemon's admission count, which excludes rate-limited sessions:
  // they hold no slot, so counting them as live would contradict the queue.
  const live = state.sessions.filter((session) => !["exited", "queued", "rate-limited"].includes(session.status)).length;
  const limited = state.sessions.filter((session) => session.status === "rate-limited");
  const queued = state.sessions.filter((session) => session.status === "queued").length;
  const needsAttention = state.sessions.filter((session) => ["needs-you", "needs-approval", "awaiting-input"].includes(session.status)).length;
  const working = state.sessions.filter((session) => ["working", "thinking"].includes(session.status)).length;
  const reporting = state.sessions.filter((session) => session.cost_usd !== null && session.cost_usd !== undefined);
  const spend = reporting.reduce((total, session) => total + (Number(session.cost_usd) || 0), 0);
  const spendLabel = reporting.length ? `$${spend.toFixed(2)}` : "—";
  // A price table has a date; a figure priced against an unnamed table cannot be
  // checked. Say which one produced this number.
  const pricingVersion = state.admission.pricing_version || "no price table";
  // A price table has a date, and until now nothing aged it: a table months out
  // of date reported spend with exactly the same confidence as a current one.
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
    ? '<span class="summary-separator">/</span><span class="summary-item summary-limited" title="' + escapeHtml(rateLimitTitle(limited, t)) + '">' + escapeHtml(countMessage("count-rate-limited", limited.length)) + "</span>"
    : "";
  // Headroom, not refusal. The agents report this continuously and the fleet
  // used to drop it at this boundary, so it could only say "rate limited" after
  // work had already stopped - with the number that would have warned first
  // sitting on the session all along.
  const quota = quotaLabel(state.sessions, t);
  const quotaSummary = '<span class="summary-separator">/</span><span class="summary-item'
    + (quota && quota.percent >= 80 ? " summary-limited" : "")
    + '" title="' + escapeHtml(quota ? quota.title : quotaUnreportedLabel(t)) + '">'
    + escapeHtml(quota ? t("fleet-quota", { percent: quota.percent }) : t("fleet-quota-unreported"))
    + "</span>";
  const summaryMarkup =
    '<span class="summary-item"><b>' + live + "/" + maxLive + "</b> " + escapeHtml(t("fleet-live")) + "</span>" +
    '<span class="summary-separator">/</span><span class="summary-item">' + escapeHtml(countMessage("count-queued", queued)) + "</span>" +
    '<span class="summary-separator">/</span><span class="summary-item summary-attention">' + escapeHtml(countMessage("count-needs-attention", needsAttention)) + "</span>" +
    '<span class="summary-separator">/</span><span class="summary-item">' + escapeHtml(countMessage("count-active", working)) + "</span>" +
    limitedSummary +
    quotaSummary +
    '<span class="summary-separator">/</span><button type="button" class="summary-item summary-spend' + (ceiling && ceiling.blocked ? " summary-limited" : "") + '" id="fleet-spend" title="' + escapeHtml(spendTitle + " " + ceilingTitle) + '" aria-label="' + escapeHtml(t("button-open-rollup")) + '"><b>' + spendLabel + "</b> " + escapeHtml(t("fleet-spent")) + (ceiling ? " " + escapeHtml("(" + ceiling.percent + "% of cap)") : "") + "</button>";
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
    return '<span class="state-chip tone-' + escapeHtml(meta.tone) + '" role="listitem" title="' + escapeHtml(metaLabel(meta)) + ': ' + escapeHtml(counts[status]) + '" aria-label="' + escapeHtml(metaLabel(meta)) + ': ' + escapeHtml(counts[status]) + '"><span class="state-chip-glyph" aria-hidden="true">' + meta.glyph + '</span><b>' + counts[status] + '</b><span>' + escapeHtml(t(meta.short)) + "</span></span>";
  }).join("");
  const stateStrip = $("fleet-state-strip");
  if (stateStrip.innerHTML !== stateMarkup) stateStrip.innerHTML = stateMarkup;
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

const GROUP_MODES = ["none", "folder", "agent", "status"];
const STATUS_FILTERS = {
  all: () => true,
  attention: (session) => isAttention(session),
  working: (session) => ["working", "thinking"].includes(session.status),
  idle: (session) => session.status === "idle",
  blocked: (session) => session.status === "rate-limited",
  exited: (session) => session.status === "exited",
};

/// Which group a session belongs to under the current mode.
function groupOf(session) {
  switch (state.groupBy) {
    case "folder":
      return folderLabel(session.cwd);
    case "agent":
      return session.agent === "codex" ? "Codex" : "Claude Code";
    case "status":
      return statusLabel(session.status);
    default:
      return "";
  }
}

/// Apply the structured filters. Returns true when the session survives.
function passesFilters(session) {
  if (state.agentFilter !== "all" && session.agent !== state.agentFilter) return false;
  const status = STATUS_FILTERS[state.statusFilter] ?? STATUS_FILTERS.all;
  return status(session);
}

/// Order sessions so members of a group are adjacent, without disturbing the
/// attention-first ordering inside each group — a blocked session must not sink
/// just because its folder sorts late.
function applyGrouping(sessions) {
  if (state.groupBy === "none") return sessions;
  const groups = new Map();
  for (const session of sessions) {
    const key = groupOf(session);
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(session);
  }
  // Groups are ordered by their most urgent member, so the folder holding a
  // session that needs you appears first.
  const ranked = [...groups.entries()].sort((left, right) => {
    const urgency = (entries) => Math.max(...entries.map((s) => STATUS_ORDER[s.status] ?? 0));
    const delta = urgency(right[1]) - urgency(left[1]);
    return delta !== 0 ? delta : left[0].localeCompare(right[0]);
  });
  return ranked.flatMap(([, entries]) => entries);
}

function syncFilterControls() {
  const group = $("group-toggle");
  group.textContent = t(`group-${state.groupBy}`);
  group.classList.toggle("wide-toggle-active", state.groupBy !== "none");
  group.setAttribute("aria-pressed", String(state.groupBy !== "none"));
  for (const [id, value] of [
    ["agent-filter", state.agentFilter],
    ["status-filter", state.statusFilter],
  ]) {
    const select = $(id);
    if (select && select.value !== value) select.value = value;
  }
}

/// A chip naming the row's group, shown only while grouping is on.
///
/// Group *headers* are deliberately not inserted into the list: it is an ARIA
/// listbox, and a child that is not an option breaks its semantics and its
/// keyboard model. Labelling each row keeps both.
function groupChip(session) {
  if (state.groupBy === "none") return "";
  const group = groupOf(session);
  if (!group) return "";
  return `<span class="row-group" title="${escapeHtml(t("row-group"))}">${escapeHtml(group)}</span>`;
}

function renderRows() {
  syncReviewVisibility();
  renderSummary();
  renderAuthBanner();
  renderPinnedSplit();
  const filter = $("filter-input").value.trim().toLowerCase();
  const desiredSessions = sortedSessions().filter((session) => {
    if (state.attentionOnly && !isAttention(session)) return false;
    if (!passesFilters(session)) return false;
    if (!filter) return true;
    return [session.name, session.cwd, folderLabel(session.cwd), session.branch, session.agent, session.model, session.status, session.phase, lifecycleLabel(session), lastActivity(session), toolProgress(session.tool_progress), session.restarts, ports(session.ports)]
      .filter(Boolean)
      .join(" ")
      .toLowerCase()
      .includes(filter);
  });
  syncFilterControls();
  const grouped = applyGrouping(desiredSessions);
  const pendingPriorityMoves = pendingPriorityChanges(grouped);
  if (state.orderFreeze) state.orderFreeze.pending = pendingPriorityMoves;
  const sessions = applyFrozenOrder(grouped);
  renderOrderNotice(pendingPriorityMoves);
  const list = $("fleet-list");
  $("empty-state").classList.toggle("empty-state-hidden", state.snapshotLoading || state.sessions.length > 0);
  list.classList.toggle("fleet-list-hidden", state.sessions.length === 0);
  list.classList.toggle("fleet-list-wide", state.wideMode);
  const identityLabel = $("column-identity-label");
  const identityLabelKey = state.wideMode ? "column-label-wide" : "column-label-compact";
  identityLabel.setAttribute("data-i18n", identityLabelKey);
  identityLabel.textContent = t(identityLabelKey);
  const wideToggle = $("wide-toggle");
  const wideTitle = state.wideMode ? "button-hide-model-effort-cost" : "button-show-model-effort-cost";
  wideToggle.setAttribute("aria-pressed", String(state.wideMode));
  wideToggle.textContent = state.wideMode ? t("button-compact") : t("button-wide");
  wideToggle.setAttribute("data-i18n-title", wideTitle);
  wideToggle.title = t(wideTitle);
  wideToggle.classList.toggle("wide-toggle-active", state.wideMode);
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
  $("fleet-order-message").textContent = countMessage("count-changed-priority", changedCount);
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
  const detail = lifecycleDetail(session);
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
  glyph.className = `status-glyph tone-${lifecycleTone(session, meta)}`;
  glyph.title = detail ? `${label} — ${detail}` : label;
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
  const statusLabelNode = row.querySelector(".row-status-label");
  statusLabelNode.textContent = label;
  statusLabelNode.title = detail || label;
  const portBadge = row.querySelector(".row-ports");
  portBadge.textContent = `${t("action-allocated-ports")} ${ports(session.ports)}`;
  reconcileGroupChip(
    row,
    state.groupBy === "none" ? "" : groupOf(session),
    t("row-group"),
  );

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
  const queueButton = row.querySelector('[data-action="queue"]');
  queueButton.textContent = queueGlyph(session);
  queueButton.classList.toggle("row-action-active", session.queued_prompts > 0);
  // A paused queue is the one state worth colouring: it means the queue has
  // stopped and is waiting for the operator, which is invisible from a count.
  queueButton.classList.toggle("row-action-warn", Boolean(session.queue_paused));
  const queueLabel = queueTitle(session);
  queueButton.title = queueLabel;
  queueButton.setAttribute("aria-label", queueLabel);
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
  const memoryCell = wideMeta.querySelector("[data-row-memory]");
  memoryCell.textContent = memory(session.memory_bytes);
  memoryCell.classList.toggle("row-memory-limited", Boolean(session.memory_limited));
  memoryCell.title = session.memory_limited ? t("memory-limited-explained") : t("memory-explained");

  // A question the agent will answer for itself has a deadline. Saying how much
  // of it is left is the difference between "answer this" and "answer this in
  // the next twelve seconds or it decides for you".
  const countdown = row.querySelector(".row-answer-deadline");
  const countdownLabel = answerCountdownLabel(session);
  if (countdown) {
    countdown.hidden = !countdownLabel;
    countdown.textContent = countdownLabel;
    if (countdownLabel) countdown.title = t("answer-deadline-explained");
  }

  const reply = row.querySelector(".row-reply");
  const replyInput = row.querySelector("input[data-reply]");
  reply.hidden = !isAttention(session) && !replyInput.value;
  replyInput.setAttribute("aria-label", t("action-reply", { name: session.name }));
  row.querySelector(".row-reply-send").setAttribute("aria-label", t("action-send-reply", { name: session.name }));
}

function isAttention(session) {
  return ["needs-approval", "awaiting-input", "needs-you"].includes(session.status);
}

/// How long the agent waits for an answer before proceeding without one.
///
/// Claude Code's `AskUserQuestion` continues after sixty seconds. Mirrors
/// `AGENT_AUTO_RESOLVE_DEADLINE` in `crates/terminalai-core/src/notification.rs`;
/// the notification grace periods on that side are measured against it.
const AGENT_AUTO_RESOLVE_SECONDS = 60;

/// Only the states the agent will answer for itself expire. A permission
/// request waits for the operator indefinitely, so counting down on one would
/// invent a deadline that does not exist.
function expiresWithoutAnAnswer(session) {
  return ["awaiting-input", "needs-you"].includes(session?.status);
}

/// Seconds left before the agent proceeds on its own, or null when nothing is
/// counting down. Never negative: a question past its deadline reads as gone
/// rather than as time owed.
function answerSecondsRemaining(session, now = Date.now()) {
  if (!expiresWithoutAnAnswer(session)) return null;
  const since = systemTimeMs(session?.status_since);
  if (!Number.isFinite(since)) return null;
  const elapsed = (now - since) / 1000;
  if (elapsed < 0) return AGENT_AUTO_RESOLVE_SECONDS;
  return Math.max(0, Math.round(AGENT_AUTO_RESOLVE_SECONDS - elapsed));
}

/// The row's countdown, or "" when the session is not waiting on an answer.
function answerCountdownLabel(session, now = Date.now()) {
  const remaining = answerSecondsRemaining(session, now);
  if (remaining === null) return "";
  return remaining > 0
    ? t("answer-deadline", { seconds: remaining })
    : t("answer-deadline-passed");
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

/// Bound here rather than at the top of the file: the renderer closes over
/// `state` and seventeen helpers, and this is the point where every one of them
/// is declared. The markup itself lives in `rowMarkup.js` — it was the longest
/// and least reviewable code in the tree.
const renderRow = createRowRenderer({
  STATUS_META,
  state,
  answerCountdownLabel,
  contextClass: contextTone,
  contextText: (session) => contextTitle(session, t),
  contextValue: contextLabel,
  cost,
  dwell,
  escapeHtml,
  folderLabel,
  groupChip,
  isAttention,
  lastActivity,
  lifecycleDetail,
  lifecycleLabel,
  lifecycleTone,
  memory,
  ports,
  queueGlyph,
  t,
  toolProgress,
});

function applySessionUpdate(session, announce = true) {
  const index = state.sessions.findIndex((item) => item.id === session.id);
  const previous = index === -1 ? null : state.sessions[index];
  if (index === -1) state.sessions.push(session);
  else state.sessions[index] = session;
  if (announce && previous) announceStatusChange(session, previous.status);
  renderRows();
  renderApprovalInbox();
  updateTerminalHeader();
}

function updateSession(session) {
  if (state.snapshotLoading) state.snapshotEvents.push({ kind: "session-updated", session });
  applySessionUpdate(session);
}

function applySessionRemoval(id) {
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

function removeSession(id) {
  if (state.snapshotLoading) state.snapshotEvents.push({ kind: "session-removed", id });
  applySessionRemoval(id);
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
  $("terminal-status").textContent = t("terminal-status-detail", {
    status: label,
    dwell: dwell(session.status_since),
    agent: session.agent === "codex" ? "Codex" : "Claude Code",
  });
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

/// What the agent said about itself, kept in its own words.
///
/// `claude agents --json` returns `state` (working | blocked | done | failed |
/// stopped) and, when blocked, `waitingFor`. The panel already ran that command
/// and threw both away, so a row said "Running" while the agent had reported it
/// was waiting on a permission prompt. Returns "" when the agent said nothing,
/// which leaves the row showing process liveness alone — never idle.
function externalReportedLabel(session) {
  const state = String(session?.reported_state ?? "").trim();
  if (!state) return "";
  const waiting = String(session?.waiting_for ?? "").trim();
  return waiting ? t("external-blocked-on", { state, waiting }) : state;
}

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
        ? '<span class="external-overlap" title="' + escapeHtml(t("external-same-folder")) + '">' + escapeHtml(t("external-same-folder-short")) + "</span>"
        : "";
      // The agent's own vocabulary, not ours. Process liveness says the thing is
      // running; only this says whether it is blocked on a permission prompt,
      // and collapsing the two made every live row read "Running".
      const reported = externalReportedLabel(session);
      const stateText = reported ? `${metaLabel(meta)} · ${reported}` : metaLabel(meta);
      const externalAriaLabel = `${label}, ${stateText}, ${countMessage("count-external", 1)}`;
      return `<article class="external-row" role="listitem" aria-label="${escapeHtml(externalAriaLabel)}">
        <span class="status-glyph tone-${escapeHtml(meta.tone)}" aria-hidden="true">◦</span>
        <div class="external-identity"><div class="external-name">${escapeHtml(label)}</div><div class="external-meta"><span title="${escapeHtml(String(session.cwd ?? ""))}">${escapeHtml(folderLabel(session.cwd))}</span><span>${escapeHtml(where)}</span>${session.version ? `<span>v${escapeHtml(session.version)}</span>` : ""}</div></div>
        <span class="external-state"${reported ? ` title="${escapeHtml(t("external-reported-by-agent"))}"` : ""}>${escapeHtml(stateText)}</span>
        <span class="external-pid" title="${escapeHtml(t("external-process-id"))}">${escapeHtml(String(session.pid))}</span>
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
    state.externalError = t("external-load-error", { error: String(error) });
  }
  renderExternal();
}

async function loadReview() {
  try {
    const snapshot = await invoke("review_snapshot");
    state.reviews = snapshot.entries ?? [];
    state.reviewError = null;
  } catch (error) {
    state.reviews = [];
    state.reviewError = String(error);
  }
  renderReview();
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
    // Read at land time, not captured earlier: the operator may tick it while
    // reading the diff.
    const archiveOnSuccess = Boolean($("review-archive-on-land")?.checked);
    const outcome = await invoke("land_session", {
      request: {
        source: cwd,
        target: cwd,
        // Named so a successful landing is recorded on the row it came from —
        // the one fact that separates a finished session from an abandoned one.
        session: id,
        archive_on_success: archiveOnSuccess,
        // Pinned to what this review described, so a target that moved while
        // the operator was reading is refused rather than silently landed on.
        expected_target_head: entry.target_head ?? null,
        verify: [],
      },
    });
    if (outcome.outcome === "landed") {
      // The work landed either way. A refused archive is reported rather than
      // swallowed, because it names something the operator can act on — the
      // session is still running, or its worktree holds unmerged commits.
      showToast(landedText(outcome), outcome.archive?.archive === "refused" ? "" : "success");
      await loadReview();
      return;
    }
    // A refusal names one specific condition. Keep the whole reason on the
    // review entry: a toast disappears while the operator is still reading.
    const reason = t("review-land-refused", { reason: refusalText(outcome) });
    entry.land_error = reason;
    renderReview();
  } catch (error) {
    entry.land_error = t("review-land-refused", { reason: String(error) });
    renderReview();
  } finally {
    if (button) {
      button.disabled = false;
      button.textContent = t("review-land");
    }
  }
}

/// Turn a structured refusal into one line the operator can act on.
/// What a successful landing did, including what became of its session.
function landedText(outcome) {
  const files = outcome.files_changed;
  if (outcome.archive?.archive === "archived") return t("review-landed-archived", { files });
  if (outcome.archive?.archive === "refused")
    return t("review-landed-not-archived", { files, reason: outcome.archive.detail });
  return t("review-landed", { files });
}

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
    showToast(t("review-marked"), "success");
  } catch (error) {
    if (button) button.disabled = false;
    showToast(t("review-mark-error", { error: String(error) }));
  }
}

async function loadSnapshot() {
  const snapshotPromise = state.snapshotQueue.then(() => loadSnapshotNow());
  state.snapshotQueue = snapshotPromise.catch(() => {});
  return snapshotPromise;
}

async function loadSnapshotNow() {
  state.snapshotLoading = true;
  state.snapshotEvents = [];
  renderSnapshotLoading();
  try {
    const snapshot = await invoke("fleet_snapshot");
    const pendingEvents = state.snapshotEvents;
    state.snapshotEvents = [];
    state.sessions = snapshot.sessions ?? [];
    state.focused = snapshot.focused ?? null;
    state.admission = snapshot.admission ?? state.admission;
    const storeQuarantine = snapshot.store_quarantine ?? null;
    if (storeQuarantine !== state.storeQuarantine) state.storeQuarantineDismissed = false;
    state.storeQuarantine = storeQuarantine;
    state.storeWriteError = snapshot.store_write_error ?? null;
    for (const event of pendingEvents) {
      if (event.kind === "session-updated") applySessionUpdate(event.session, false);
      if (event.kind === "session-removed") applySessionRemoval(event.id);
    }
    // Events arriving while the focused channel is reattached belong to this
    // already-reconciled state and must be applied live, not buffered again.
    state.snapshotLoading = false;
    renderSnapshotLoading();
    renderStoreQuarantine();
    renderStoreWriteError();
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
  // The app-side output registry keeps only the newest route. Serializing the
  // attach/restore sequence prevents rapid arrow navigation from letting an
  // older request register its channel after a newer request has completed.
  const switchPromise = state.focusQueue.then(() => focusSessionNow(id));
  state.focusQueue = switchPromise.catch(() => {});
  return switchPromise;
}

async function focusSessionNow(id) {
  const previousFocused = state.focused;
  state.focused = id;
  state.focusGeneration += 1;
  state.terminal?.reset();
  fitTerminal();
  renderRows();
  updateTerminalHeader();
  try {
    await attachSessionOutput(id);
    if (state.focused !== id) return;
    updateTerminalHeader();
    renderRows();
  } catch (error) {
    // A later focus request or a session removal owns the pane now. An older
    // failure must not roll that state back.
    if (state.focused !== id) return;
    state.focused = previousFocused;
    state.focusGeneration += 1;
    state.outputChannel = null;
    renderRows();
    updateTerminalHeader();
    try {
      if (previousFocused) await attachSessionOutput(previousFocused);
    } catch (restoreError) {
      state.outputChannel = null;
      showToast(t("focus-session-error", { error: `${String(error)}; ${String(restoreError)}` }));
      return;
    }
    if (state.focused !== previousFocused) return;
    updateTerminalHeader();
    renderRows();
    showToast(t("focus-session-error", { error: String(error) }));
  }
}

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

  const tokenCells = (row) =>
    TOKEN_FIELDS.map(([field]) => `<td class="rollup-number">${escapeHtml(formatTokens(row[field]))}</td>`).join("");
  const groupTable = (titleKey, rows, label = (row) => row.key) => `
    <section class="rollup-section">
      <h3>${escapeHtml(t(titleKey))}</h3>
      <table class="rollup-table">
        <thead><tr><th>${escapeHtml(t(titleKey))}</th><th class="rollup-number">$</th>${TOKEN_FIELDS.map(([, key]) => `<th class="rollup-number">${escapeHtml(t(key))}</th>`).join("")}</tr></thead>
        <tbody>${rows
          .map(
            (row) => `<tr><th scope="row">${escapeHtml(label(row))}${
              row.unpriced ? `<small class="rollup-unpriced"> +${row.unpriced}</small>` : ""
            }</th><td class="rollup-number">${escapeHtml(formatCost(row.priced ? row.cost_usd : null))}</td>${tokenCells(row)}</tr>`,
          )
          .join("")}</tbody>
      </table>
    </section>`;

  // Sessions are their own grouping so a single expensive run is visible rather
  // than hidden inside its folder's subtotal.
  const bySession = rollupBy(sessions, (session) => session.id).map((row) => {
    const session = sessions.find((item) => item.id === row.key);
    return { ...row, label: session ? `${session.id} · ${session.name}` : row.key };
  });

  $("rollup-body").innerHTML = [
    groupTable("rollup-by-agent", rollupBy(sessions, (session) => session.agent)),
    groupTable("rollup-by-folder", rollupBy(sessions, folderOf)),
    groupTable("rollup-by-session", bySession, (row) => row.label),
    `<section class="rollup-section rollup-total"><h3>${escapeHtml(t("rollup-total"))}</h3><p><b>${escapeHtml(
      formatCost(totals.priced ? totals.cost_usd : null),
    )}</b> · ${escapeHtml(String(totals.requests))} ${escapeHtml(t("rollup-requests"))}</p></section>`,
  ].join("");
}

function openRollup() {
  renderRollup();
  const dialog = $("rollup-dialog");
  if (!dialog.open) dialog.showModal();
}

/**
 * Send one prompt to several sessions.
 *
 * Every session is listed, ineligible ones included and greyed with the reason,
 * because hiding them makes the fleet look smaller than it is at the moment the
 * operator is deciding who to send to. Only eligible ones start ticked.
 */
function renderBroadcast() {
  const rows = targets(state.sessions);
  const eligible = rows.filter((row) => !row.reason).length;
  $("broadcast-coverage").textContent = eligible
    ? t("broadcast-eligible", { count: eligible })
    : t("broadcast-none-eligible");
  $("broadcast-list").innerHTML = rows
    .map(({ session, reason }) => {
      const checked = !reason && state.broadcastSelection.includes(session.id) ? " checked" : "";
      const disabled = reason ? " disabled" : "";
      const why = reason ? `<small class="broadcast-why">${escapeHtml(t(reason))}</small>` : "";
      return `<label class="broadcast-row${reason ? " is-ineligible" : ""}"><input type="checkbox" data-broadcast-id="${escapeHtml(session.id)}"${checked}${disabled} /><span>${escapeHtml(session.id)} · ${escapeHtml(session.name)}</span>${why}</label>`;
    })
    .join("");
  $("send-broadcast-button").disabled = eligible === 0;
}

function openBroadcast() {
  state.broadcastSelection = defaultSelection(state.sessions);
  renderBroadcast();
  const dialog = $("broadcast-dialog");
  if (!dialog.open) dialog.showModal();
}

function readBroadcastSelection() {
  return [...$("broadcast-list").querySelectorAll("input[data-broadcast-id]")]
    .filter((box) => box.checked && !box.disabled)
    .map((box) => box.dataset.broadcastId);
}

function syncBroadcastSelection() {
  state.broadcastSelection = readBroadcastSelection();
}

async function sendBroadcast() {
  const text = $("broadcast-input").value.trim();
  if (!text) {
    showToast(t("broadcast-empty-prompt"));
    return;
  }
  // Re-checked at send time rather than trusted from when the dialog opened: a
  // session can enter a permission prompt while the operator is typing, and the
  // daemon would refuse it anyway.
  const ids = readBroadcastSelection().filter((id) =>
    isEligible(state.sessions.find((session) => session.id === id)),
  );
  if (!ids.length) {
    showToast(t("broadcast-none-eligible"));
    return;
  }
  try {
    const results = await invoke("broadcast_prompt", { ids, text });
    const { delivered, refused, total } = summarize(results);
    // Both numbers, always. "Sent" alone, when four of nine were skipped, is
    // the failure the per-session protocol exists to prevent.
    const message = refused
      ? `${t("broadcast-sent", { delivered, total })} · ${t("broadcast-refused", { count: refused })}`
      : t("broadcast-sent", { delivered, total });
    showToast(message, refused ? "" : "success");
    if (!refused) {
      $("broadcast-input").value = "";
      $("broadcast-dialog").close();
    } else {
      // Re-rendering after a partial refusal must preserve the boxes the
      // operator left checked, rather than restoring the open-time default.
      syncBroadcastSelection();
      renderBroadcast();
    }
  } catch (error) {
    showToast(t("broadcast-error", { error: String(error) }));
  }
}

/**
 * Which projects still have roadmap work.
 *
 * Every cell distinguishes "unknown" from "none": a project with no roadmap,
 * and one whose roadmap is prose rather than checkboxes, both have an unknown
 * amount queued, and rendering either as 0 would sort it beside a finished
 * project and quietly remove it from consideration.
 */
function renderProjects() {
  if (state.projectsError) {
    $("projects-coverage").textContent = t("projects-unavailable");
    renderDataError(
      $("projects-body"),
      t("projects-load-error", { error: state.projectsError }),
      "projects",
      openProjects,
    );
    return;
  }
  const openOnly = $("projects-open-only").checked;
  const all = state.scannedProjects;
  const rows = sortProjects(openOnly ? all.filter((item) => hasOpenWork(item.roadmap)) : all);
  const { withWork, unknown, total } = summarizeProjects(all);
  $("projects-coverage").textContent = total
    ? t("projects-summary", { withWork, total, unknown })
    : t("projects-none-registered");

  if (!rows.length) {
    $("projects-body").innerHTML = `<p class="rollup-total">${escapeHtml(
      total ? t("projects-none-matching") : t("projects-none-registered"),
    )}</p>`;
    return;
  }
  $("projects-body").innerHTML = `
    <table class="rollup-table projects-table">
      <thead><tr>
        <th>${escapeHtml(t("projects-column-project"))}</th>
        <th class="rollup-number">${escapeHtml(t("projects-column-open"))}</th>
        <th>${escapeHtml(t("projects-column-touched"))}</th>
        <th>${escapeHtml(t("projects-column-next"))}</th>
        <th></th>
      </tr></thead>
      <tbody>${rows
        .map((item) => {
          const cell = openItemsCell(item.roadmap, t);
          const touched = stalenessLabel(item.roadmap, t) ?? "—";
          const next = item.roadmap?.next_item ?? "";
          // Each cell is built separately so no interpolation has to wrap, and
          // every value is escaped at the point it lands — including inside an
          // attribute, which `contentSecurity.test.mjs` checks uniformly.
          const cells = [
            `<th scope="row" title="${escapeHtml(item.path)}">${escapeHtml(item.name)}</th>`,
            `<td class="rollup-number${cell.known ? "" : " rollup-unpriced"}">${escapeHtml(cell.text)}</td>`,
            `<td>${escapeHtml(touched)}</td>`,
            `<td class="projects-next">${escapeHtml(next)}</td>`,
            `<td><button type="button" class="button button-quiet" data-launch-project="${escapeHtml(item.path)}">${escapeHtml(t("projects-launch"))}</button></td>`,
          ];
          return `<tr>${cells.join("")}</tr>`;
        })
        .join("")}</tbody>
    </table>`;
  for (const button of $("projects-body").querySelectorAll("[data-launch-project]")) {
    button.addEventListener("click", () => {
      $("projects-dialog").close();
      openLauncher();
      $("cwd-input").value = button.dataset.launchProject;
      schedulePreview();
      void loadProjectTemplates();
    });
  }
}

const WORK_STATE_LABEL = {
  pending: "work-state-pending",
  running: "work-state-running",
  done: "work-state-done",
  failed: "work-state-failed",
  skipped: "work-state-skipped",
  flagged: "work-state-flagged",
  // Its own category, not a failure: nothing went wrong, the fleet was busy for
  // longer than the work was worth.
  expired: "work-state-expired",
};

/**
 * The run in progress, if there is one.
 *
 * Every category is shown, including the ones that did nothing. A run over
 * forty projects reporting only "done" is one the operator has to audit by
 * hand, which is the work they were trying to avoid.
 */
function renderWorkRun() {
  const run = state.workRun;
  const body = $("work-run-body");
  body.hidden = !run;
  $("work-pause-button").hidden = !run || run.paused;
  $("work-resume-button").hidden = !run || !run.paused;
  $("work-clear-button").hidden = !run;
  if (!run) return;

  const counts = run.entries.reduce((totals, entry) => {
    const kind = entry.state.kind;
    totals[kind] = (totals[kind] ?? 0) + 1;
    return totals;
  }, {});
  const summary = t("work-outcome", {
    done: counts.done ?? 0,
    running: counts.running ?? 0,
    pending: counts.pending ?? 0,
    flagged: counts.flagged ?? 0,
    failed: counts.failed ?? 0,
    skipped: counts.skipped ?? 0,
    expired: counts.expired ?? 0,
  });
  const rows = run.entries
    .map((entry) => {
      const kind = entry.state.kind;
      const label = t(WORK_STATE_LABEL[kind] ?? "work-state-pending");
      const detail = workEntryDetail(entry);
      // Only a flagged entry offers a decision; everything else is a report.
      const actions =
        kind === "flagged"
          ? `<span><button type="button" class="button button-quiet" data-work-approve="${escapeHtml(entry.project)}">${escapeHtml(t("work-approve"))}</button><button type="button" class="button button-quiet" data-work-skip="${escapeHtml(entry.project)}">${escapeHtml(t("work-skip"))}</button></span>`
          : "<span></span>";
      return `<div class="work-entry work-entry-${escapeHtml(kind)}"><span title="${escapeHtml(entry.project)}">${escapeHtml(entry.name)}</span><span class="work-entry-state" title="${escapeHtml(detail)}">${escapeHtml(label)}</span>${actions}</div>`;
    })
    .join("");
  body.innerHTML = `<p class="rollup-total">${escapeHtml(run.paused ? `${t("work-run-paused")} · ${summary}` : summary)}</p>${rows}`;
  for (const button of body.querySelectorAll("[data-work-approve]")) {
    button.addEventListener("click", () => void workEntryAction("approve_flagged_project", button.dataset.workApprove));
  }
  for (const button of body.querySelectorAll("[data-work-skip]")) {
    button.addEventListener("click", () => void workEntryAction("skip_work_project", button.dataset.workSkip));
  }
}

/** Why an entry is in the state it is, for the row's tooltip. */
function workEntryDetail(entry) {
  const state = entry.state;
  if (state.kind === "failed") return state.detail ?? "";
  if (state.kind === "flagged") {
    const tree = state.tree ?? {};
    if (tree.kind === "dirty") return t("work-dirty-detail", { count: tree.files?.length ?? 0 });
    if (tree.kind === "unknown") return `${t("work-tree-unknown")}: ${tree.detail ?? ""}`;
  }
  if (state.kind === "running" || state.kind === "done") return state.session ?? "";
  if (state.kind === "expired") {
    return t("work-expired-detail", { minutes: Math.round(Number(state.waited_seconds ?? 0) / 60) });
  }
  return "";
}

async function workEntryAction(command, path) {
  try {
    await invoke(command, { path });
  } catch (error) {
    showToast(String(error));
  }
  await refreshWorkRun();
}

async function setWorkRunPaused(paused) {
  try {
    await invoke("set_work_run_paused", { paused });
  } catch (error) {
    showToast(String(error));
  }
  await refreshWorkRun();
}

async function refreshWorkRun() {
  try {
    state.workRun = await invoke("work_run");
  } catch (error) {
    state.workRun = null;
    showToast(String(error));
  }
  renderWorkRun();
}

function renderPromptLibrary() {
  const list = $("stored-prompt-list");
  if (state.storedPromptsError) {
    $("prompt-library-count").textContent = t("prompt-library-unavailable");
    renderDataError(
      list,
      t("prompt-library-load-error", { error: state.storedPromptsError }),
      "prompt-library",
      loadStoredPrompts,
    );
    return;
  }
  $("prompt-library-count").textContent = t("prompt-library-count", {
    count: state.storedPrompts.length,
  });
  if (!state.storedPrompts.length) {
    list.innerHTML =
      '<p class="rollup-total">' + escapeHtml(t("prompt-library-empty")) + "</p>";
    return;
  }
  list.innerHTML = state.storedPrompts
    .map((prompt) => {
      const name = String(prompt.name ?? "");
      const selectLabel = t("prompt-select", { name });
      const deleteLabel = t("prompt-delete", { name });
      const source = prompt.source ? t("prompt-source-seeded") : t("prompt-source-local");
      const active = name === state.activeStoredPrompt;
      return '<div class="stored-prompt-row" role="listitem"><button type="button" class="stored-prompt-select" data-prompt-select="' +
        escapeHtml(name) +
        '" aria-pressed="' +
        String(active) +
        '" aria-label="' +
        escapeHtml(selectLabel) +
        '"><span>' +
        escapeHtml(name) +
        "</span><small>" +
        escapeHtml(source) +
        '</small></button><button type="button" class="row-action row-action-danger" data-prompt-delete="' +
        escapeHtml(name) +
        '" title="' +
        escapeHtml(deleteLabel) +
        '" aria-label="' +
        escapeHtml(deleteLabel) +
        '">×</button></div>';
    })
    .join("");
}

function newStoredPrompt() {
  state.activeStoredPrompt = null;
  $("stored-prompt-name").value = "";
  $("stored-prompt-text").value = "";
  renderPromptLibrary();
}

function selectStoredPrompt(name) {
  const prompt = state.storedPrompts.find((entry) => entry.name === name);
  if (!prompt) {
    newStoredPrompt();
    return;
  }
  state.activeStoredPrompt = prompt.name;
  $("stored-prompt-name").value = prompt.name;
  $("stored-prompt-text").value = prompt.text;
  renderPromptLibrary();
}

async function saveStoredPrompt() {
  const name = $("stored-prompt-name").value.trim();
  const text = $("stored-prompt-text").value;
  if (!name) {
    showToast(t("prompt-name-required"));
    $("stored-prompt-name").focus();
    return;
  }
  if (!text.trim()) {
    showToast(t("prompt-text-required"));
    $("stored-prompt-text").focus();
    return;
  }
  const previous = state.activeStoredPrompt;
  try {
    await invoke("save_stored_prompt", {
      prompt: { name, text, source: null },
    });
    if (previous && previous !== name) {
      await invoke("delete_stored_prompt", { name: previous });
    }
    state.activeStoredPrompt = name;
    await loadStoredPrompts();
    selectStoredPrompt(name);
    showToast(t("prompt-saved", { name }), "success");
  } catch (error) {
    showToast(String(error));
  }
}

async function deleteStoredPrompt(name) {
  if (!name) return;
  try {
    const removed = await invoke("delete_stored_prompt", { name });
    if (removed && state.activeStoredPrompt === name) newStoredPrompt();
    await loadStoredPrompts();
    showToast(
      removed ? t("prompt-deleted", { name }) : t("prompt-not-found", { name }),
      removed ? "success" : "",
    );
  } catch (error) {
    showToast(String(error));
  }
}

async function loadStoredPrompts() {
  try {
    state.storedPrompts = await invoke("list_stored_prompts");
    state.storedPromptsError = null;
  } catch (error) {
    state.storedPrompts = [];
    state.storedPromptsError = String(error);
  }
  const select = $("work-prompt-select");
  const selected = select.value;
  select.innerHTML = state.storedPrompts
    .map((prompt) => '<option value="' + escapeHtml(prompt.name) + '">' + escapeHtml(prompt.name) + "</option>")
    .join("");
  // With nothing stored there is nothing to run, and a button that always
  // errors is worse than one that is plainly unavailable.
  const empty = state.storedPrompts.length === 0;
  $("work-start-button").disabled = empty;
  select.disabled = empty;
  if (!empty && state.storedPrompts.some((prompt) => prompt.name === selected)) {
    select.value = selected;
  }
  if (empty) {
    select.innerHTML = '<option value="">' + escapeHtml(t("work-no-prompts")) + "</option>";
  }
  if (
    state.activeStoredPrompt &&
    !state.storedPrompts.some((prompt) => prompt.name === state.activeStoredPrompt)
  ) {
    newStoredPrompt();
  } else {
    renderPromptLibrary();
  }
}

function openPromptLibrary() {
  const dialog = $("prompt-dialog");
  if (!dialog.open) dialog.showModal();
  void loadStoredPrompts();
}

/**
 * Run the chosen prompt across the projects currently listed.
 *
 * Deliberately the *listed* projects rather than all known ones: the filter
 * above the table is how the operator says which they mean, and a button that
 * ignored it would launch agents in repositories they had just filtered out.
 */
async function startWorkRun() {
  const prompt = $("work-prompt-select").value;
  if (!prompt) return;
  const openOnly = $("projects-open-only").checked;
  const listed = openOnly
    ? state.scannedProjects.filter((item) => hasOpenWork(item.roadmap))
    : state.scannedProjects;
  if (!listed.length) {
    showToast(t("projects-none-matching"));
    return;
  }
  try {
    state.workRun = await invoke("start_work_run", {
      prompt,
      projects: listed.map((item) => item.path),
    });
    showToast(t("work-started", { count: listed.length }), "success");
  } catch (error) {
    showToast(String(error));
  }
  renderWorkRun();
}

/// Show every session waiting on a decision.
///
/// A view over the snapshot the window already holds — a blocked session
/// carries its own pending request — so there is no separate poll and nothing
/// here can disagree with the rows behind it.
function openApprovals() {
  const dialog = $("approvals-dialog");
  if (!dialog.open) dialog.showModal();
  renderApprovalInbox();
}

function renderApprovalInbox() {
  const dialog = $("approvals-dialog");
  if (!dialog.open) return;
  const waiting = pendingApprovals(state.sessions);
  $("approvals-count").textContent = t("approvals-count", { count: waiting.length });
  const body = $("approvals-body");
  // The inbox follows the fleet, so it re-renders whenever any session
  // changes — and a session unrelated to this one changing must not wipe an
  // answer being typed. Skipped entirely when nothing the inbox shows has
  // moved, and typed text is carried across when it has.
  const signature = waiting
    .map((session) => `${session.id}:${requestLine(session, t)}`)
    .join("|");
  if (body.dataset.signature === signature) return;
  const typed = new Map(
    [...body.querySelectorAll("[data-approval-reply]")]
      .filter((element) => element.value)
      .map((element) => [element.dataset.approvalReply, element.value]),
  );
  body.dataset.signature = signature;
  body.innerHTML = renderApprovals(waiting, {
    escape: escapeHtml,
    translate: t,
    dwell: (session) => relativeDwell(waitingSince(session) ?? systemTimeMs(session.status_since)),
  });
  for (const [id, value] of typed) {
    const input = [...body.querySelectorAll("[data-approval-reply]")].find(
      (element) => element.dataset.approvalReply === id,
    );
    if (input) input.value = value;
  }
  for (const button of body.querySelectorAll("[data-approval-focus]")) {
    button.addEventListener("click", () => {
      dialog.close();
      void focusSession(button.dataset.approvalFocus);
    });
  }
  for (const button of body.querySelectorAll("[data-approval-send]")) {
    button.addEventListener("click", () => void sendApproval(button.dataset.approvalSend));
  }
  for (const input of body.querySelectorAll("[data-approval-reply]")) {
    input.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      void sendApproval(input.dataset.approvalReply);
    });
  }
}

/// Send the operator's answer to the session that asked.
///
/// The same bracketed-paste write the fleet row's reply box uses, because it is
/// the same act: typing at that session's prompt. Nothing is auto-approved and
/// no universal "yes" is invented — what the agent accepts is its own prompt's
/// vocabulary, and guessing it would be answering on the operator's behalf.
async function sendApproval(id) {
  // Walked rather than selected: a session id in a selector string is the same
  // shape of mistake as one in markup, even though the escaper would differ.
  const input = [...$("approvals-body").querySelectorAll("[data-approval-reply]")].find(
    (element) => element.dataset.approvalReply === id,
  );
  const answer = input?.value.trim();
  if (!answer) return;
  try {
    await invoke("write_session", { id, data: `[200~${answer}[201~
` });
    await invoke("mark_read", { id });
    input.value = "";
    showToast(t("approvals-sent"), "success");
  } catch (error) {
    showToast(String(error));
  }
}

/// Show the saved layouts.
async function openWorkingSets() {
  const dialog = $("working-sets-dialog");
  if (!dialog.open) dialog.showModal();
  await refreshWorkingSets();
}

async function refreshWorkingSets() {
  const body = $("working-sets-body");
  try {
    const sets = await invoke("list_working_sets");
    $("working-sets-count").textContent = t("working-sets-count", { count: sets.length });
    body.innerHTML = sets.length
      ? sets.map((set) => renderWorkingSet(set, { escape: escapeHtml, translate: t })).join("")
      : `<p class="rollup-total">${escapeHtml(t("working-sets-empty"))}</p>`;
  } catch (error) {
    $("working-sets-count").textContent = "";
    body.innerHTML = `<p class="rollup-total">${escapeHtml(String(error))}</p>`;
    return;
  }
  for (const button of body.querySelectorAll("[data-restore-set]")) {
    button.addEventListener("click", () => void restoreWorkingSet(button.dataset.restoreSet));
  }
  for (const button of body.querySelectorAll("[data-delete-set]")) {
    button.addEventListener("click", async () => {
      try {
        await invoke("delete_working_set", { name: button.dataset.deleteSet });
        await refreshWorkingSets();
      } catch (error) {
        showToast(String(error));
      }
    });
  }
}

/// Relaunch a layout and show, per session, what the fleet decided.
///
/// The outcomes are rendered in place rather than toasted: a restore of twelve
/// sessions can have several distinct refusals, and a toast holds one line for
/// four seconds.
async function restoreWorkingSet(name) {
  // Found by walking the elements rather than by building a selector string:
  // a layout name is operator input, and interpolating it into a selector is
  // the same shape of mistake as interpolating it into markup even though the
  // escaper would differ.
  const list = [...$("working-sets-body").querySelectorAll("[data-outcomes-for]")].find(
    (element) => element.dataset.outcomesFor === name,
  );
  if (list) list.innerHTML = `<li>${escapeHtml(t("loading"))}</li>`;
  try {
    const outcomes = await invoke("restore_working_set", { name });
    if (list) {
      list.innerHTML = renderRestoreOutcomes(outcomes, { escape: escapeHtml, translate: t });
    }
    showToast(t("working-sets-restored", summarizeRestore(outcomes)), "success");
  } catch (error) {
    if (list) list.innerHTML = "";
    showToast(String(error));
  }
}

/// Ask the daemon which sessions printed a string, and how many times.
///
/// Find-in-pane answers the same question for the one attached renderer; this
/// is the reason the disk tier exists at all — the other twenty-nine sessions
/// have no renderer, and until now their output could only be read by focusing
/// each one in turn.
async function runFleetSearch() {
  const needle = $("fleet-search-input").value.trim();
  const body = $("fleet-search-body");
  const count = $("fleet-search-count");
  if (needle.length < 2) {
    count.textContent = "";
    body.innerHTML = `<p class="rollup-total">${escapeHtml(t("fleet-search-too-short"))}</p>`;
    return;
  }
  const button = $("fleet-search-run");
  button.disabled = true;
  body.innerHTML = `<p class="rollup-total">${escapeHtml(t("loading"))}</p>`;
  try {
    const matches = await invoke("search_fleet", {
      needle,
      caseSensitive: $("fleet-search-case").checked,
    });
    count.textContent = t("fleet-search-summary", searchSummary(matches));
    body.innerHTML = renderSearchResults(matches, {
      escape: escapeHtml,
      translate: t,
      needle,
    });
    for (const element of body.querySelectorAll("[data-search-focus]")) {
      element.addEventListener("click", () => {
        $("search-dialog").close();
        void focusSession(element.dataset.searchFocus);
      });
    }
  } catch (error) {
    count.textContent = "";
    body.innerHTML = `<p class="rollup-total">${escapeHtml(String(error))}</p>`;
  } finally {
    button.disabled = false;
  }
}

async function openSessionHistory() {
  const dialog = $("history-dialog");
  if (!dialog.open) dialog.showModal();
  $("history-body").innerHTML = `<p class="rollup-total">${escapeHtml(t("loading"))}</p>`;
  let archives = [];
  try {
    archives = await invoke("session_history");
  } catch (error) {
    $("history-count").textContent = "";
    $("history-body").innerHTML =
      `<p class="rollup-total">${escapeHtml(t("session-history-error", { error: String(error) }))}</p>`;
    return;
  }
  $("history-count").textContent = t("session-history-count", { count: archives.length });
  $("history-body").innerHTML = renderSessionHistory(archives, {
    escape: escapeHtml,
    translate: t,
    formatTime: (ms) => new Date(ms).toLocaleString(),
  });
  await refreshWorktrees();
  for (const button of $("history-body").querySelectorAll("[data-relaunch]")) {
    button.addEventListener("click", () => {
      const archive = archives.find((item) => item.id === button.dataset.relaunch);
      if (!archive) return;
      dialog.close();
      openLauncher();
      // Only what the archive actually holds. The command is kept as text, so
      // restoring a model or a sandbox from it would mean parsing an argv this
      // record never promised to keep parseable.
      $("agent-input").value = archive.agent;
      $("name-input").value = archive.name ?? "";
      $("cwd-input").value = archive.cwd ?? "";
      schedulePreview();
      void loadProjectTemplates();
    });
  }
}

/** Survey leftover checkouts inside the history dialog, which is where a
 * finished session's leavings belong. */
async function refreshWorktrees() {
  let worktrees = [];
  try {
    worktrees = await invoke("stale_worktrees");
  } catch (error) {
    $("worktrees-count").textContent = "";
    $("worktrees-body").innerHTML =
      `<p class="rollup-total">${escapeHtml(t("worktrees-error", { error: String(error) }))}</p>`;
    return;
  }
  $("worktrees-count").textContent = t("worktrees-count", { count: worktrees.length });
  $("worktrees-body").innerHTML = renderWorktrees(worktrees, { escape: escapeHtml, translate: t });
  for (const button of $("worktrees-body").querySelectorAll("[data-reap]")) {
    button.addEventListener("click", async () => {
      const stale = worktrees[Number(button.dataset.reap)];
      if (!stale) return;
      button.disabled = true;
      try {
        await invoke("reap_worktree", { stale });
        showToast(t("worktrees-removed"), "success");
      } catch (error) {
        // The core refuses unmerged and unknown states too, so a refusal that
        // reaches here is worth reading rather than retrying.
        showToast(String(error));
        button.disabled = false;
        return;
      }
      await refreshWorktrees();
    });
  }
}

async function openProjects() {
  const dialog = $("projects-dialog");
  if (!dialog.open) dialog.showModal();
  // Scanning reads a file per project, so the dialog opens first and fills in
  // rather than blocking on a few hundred reads before anything appears.
  $("projects-body").innerHTML = `<p class="rollup-total">${escapeHtml(t("loading"))}</p>`;
  try {
    state.scannedProjects = await invoke("scan_projects");
    state.projectsError = null;
  } catch (error) {
    state.scannedProjects = [];
    state.projectsError = String(error);
  }
  renderProjects();
  await Promise.all([loadProjectRoots(), loadStoredPrompts()]);
  await refreshWorkRun();
}

/** The queue button's glyph: the count when there is one, an outline when not. */
function queueGlyph(session) {
  const count = session.queued_prompts ?? 0;
  if (!count) return "≡";
  return count > 9 ? "9+" : String(count);
}

function queueTitle(session) {
  const count = session.queued_prompts ?? 0;
  if (session.queue_paused) {
    return t("queue-paused-title", { name: session.name, reason: t(`queue-pause-${session.queue_paused}`) });
  }
  return count
    ? t("queue-count-title", { name: session.name, count })
    : t("action-queue", { name: session.name });
}

/**
 * The prompts waiting on one session.
 *
 * Fetched when the dialog opens rather than carried on every row: a session is
 * re-rendered on each status change, and the prompts can be a quarter of a
 * megabyte each.
 */
async function openQueue(id) {
  state.queueSession = id;
  const dialog = $("queue-dialog");
  if (!dialog.open) dialog.showModal();
  await refreshQueue();
  $("queue-input").focus();
}

async function refreshQueue() {
  const id = state.queueSession;
  if (!id) return;
  try {
    state.queuePrompts = await invoke("queued_prompts", { id });
    state.queueError = null;
  } catch (error) {
    state.queuePrompts = [];
    state.queueError = String(error);
  }
  renderQueue();
}

function renderQueue() {
  const id = state.queueSession;
  const session = state.sessions.find((item) => item.id === id);
  $("queue-title").textContent = session ? t("queue-title", { name: session.name }) : t("queue-title-generic");
  const paused = session?.queue_paused ?? null;
  // Always stated, and always with the reason. "Paused" alone leaves the
  // operator guessing whether the agent is waiting on them or the queue is.
  $("queue-status").textContent = paused
    ? t("queue-paused-detail", { reason: t(`queue-pause-${paused}`) })
    : t("queue-running");
  $("queue-resume-button").hidden = !paused;
  $("queue-pause-button").hidden = Boolean(paused);

  if (state.queueError) {
    $("queue-status").textContent = t("queue-unavailable");
    $("queue-resume-button").hidden = true;
    $("queue-pause-button").hidden = true;
    renderDataError(
      $("queue-list"),
      t("queue-load-error", { error: state.queueError }),
      "queue",
      refreshQueue,
    );
    return;
  }

  if (!state.queuePrompts.length) {
    $("queue-list").innerHTML = `<p class="rollup-total">${escapeHtml(t("queue-empty"))}</p>`;
    return;
  }
  $("queue-list").innerHTML = state.queuePrompts
    .map((prompt, index) => {
      const position = t("queue-position", { position: index + 1 });
      return `<li class="queue-row" data-prompt="${escapeHtml(String(prompt.id))}"><span class="queue-position">${escapeHtml(position)}</span><textarea class="queue-text" rows="2" aria-label="${escapeHtml(position)}">${escapeHtml(prompt.text)}</textarea><span class="queue-row-actions"><button type="button" class="row-action" data-queue-action="up" title="${escapeHtml(t("queue-move-up"))}" aria-label="${escapeHtml(t("queue-move-up"))}">↑</button><button type="button" class="row-action" data-queue-action="down" title="${escapeHtml(t("queue-move-down"))}" aria-label="${escapeHtml(t("queue-move-down"))}">↓</button><button type="button" class="row-action" data-queue-action="save" title="${escapeHtml(t("queue-save"))}" aria-label="${escapeHtml(t("queue-save"))}">✓</button><button type="button" class="row-action row-action-danger" data-queue-action="remove" title="${escapeHtml(t("queue-withdraw"))}" aria-label="${escapeHtml(t("queue-withdraw"))}">×</button></span></li>`;
    })
    .join("");
  for (const button of $("queue-list").querySelectorAll("[data-queue-action]")) {
    button.addEventListener("click", () => void queueRowAction(button));
  }
}

async function queueRowAction(button) {
  const id = state.queueSession;
  const row = button.closest("[data-prompt]");
  const prompt = Number(row.dataset.prompt);
  const index = state.queuePrompts.findIndex((item) => item.id === prompt);
  const action = button.dataset.queueAction;
  try {
    if (action === "remove") await invoke("remove_queued_prompt", { id, prompt });
    if (action === "save") {
      await invoke("edit_queued_prompt", { id, prompt, text: row.querySelector(".queue-text").value });
      showToast(t("queue-saved"), "success");
    }
    if (action === "up" && index > 0) {
      await invoke("reorder_queued_prompt", { id, prompt, to: index - 1 });
    }
    if (action === "down" && index < state.queuePrompts.length - 1) {
      await invoke("reorder_queued_prompt", { id, prompt, to: index + 1 });
    }
  } catch (error) {
    // Usually a race: the prompt fired while the operator was deciding. The
    // backend names that case, so it is shown rather than swallowed.
    showToast(String(error));
  }
  await refreshQueue();
}

async function addQueuedPrompt() {
  const id = state.queueSession;
  const text = $("queue-input").value.trim();
  if (!text) {
    showToast(t("queue-empty-prompt"));
    return;
  }
  try {
    await invoke("enqueue_prompt", { id, text });
    $("queue-input").value = "";
  } catch (error) {
    showToast(String(error));
  }
  await refreshQueue();
}

/**
 * The in-app explanation of the row -> focused-terminal model.
 *
 * The one thing that makes this tool different is also the thing nobody
 * guesses from looking at it: a row is not a terminal. An operator who assumes
 * it is spends their first minutes wondering why clicking a row does not open
 * anything, and concludes the app is broken rather than dense on purpose.
 *
 * The state list is generated from the same table the rows are drawn from, so a
 * status added later cannot appear on a row while missing from the explanation.
 */
function openExplainer() {
  $("explainer-states").innerHTML = STATUS_KEYS.map((status) => {
    const meta = STATUS_META[status];
    return `<div class="explainer-state"><dt><span class="state-chip tone-${escapeHtml(meta.tone)}"><span class="state-chip-glyph" aria-hidden="true">${meta.glyph}</span><span>${escapeHtml(t(meta.short))}</span></span></dt><dd>${escapeHtml(t(`${meta.short}-explained`))}</dd></div>`;
  }).join("");
  const dialog = $("explainer-dialog");
  if (!dialog.open) dialog.showModal();
}

function createOutputChannel(id) {
  const generation = state.focusGeneration;
  const channel = new Channel();
  channel.onmessage = (data) => writeTerminalBytes(data, id, generation);
  return channel;
}

/// The focused pane already contains this in-memory ring after attach. Ask for
/// the ring plus one older window so the reset-and-replay path includes bytes
/// the pane did not already show.
const MAX_SCROLLBACK_BYTES = 512 * 1024;
const HISTORY_OLDER_BYTES = 128 * 1024;
const HISTORY_REQUEST_BYTES = MAX_SCROLLBACK_BYTES + HISTORY_OLDER_BYTES;

/// Show or hide the find bar over the focused pane.
///
/// Closing clears the search rather than leaving it: xterm keeps its highlight
/// decorations until told otherwise, so a hidden bar with a live search leaves
/// the pane marked up for a query the operator can no longer see.
function toggleFind(next = null) {
  const bar = $("terminal-find");
  const open = next === null ? bar.hidden : next;
  bar.hidden = !open;
  $("terminal-find-toggle").setAttribute("aria-pressed", String(open));
  if (open) {
    $("terminal-find-input").focus();
    $("terminal-find-input").select();
    runFind();
  } else {
    state.searchAddon?.clearDecorations();
    $("terminal-find-count").textContent = "";
    state.terminal?.focus();
  }
}

/// Run the current query. `direction` moves to the adjacent match; omitting it
/// re-runs in place, which is what a keystroke in the field wants.
function runFind(direction = null) {
  const needle = $("terminal-find-input").value;
  if (!state.searchAddon) return;
  if (!needle) {
    state.searchAddon.clearDecorations();
    $("terminal-find-count").textContent = "";
    return;
  }
  // Read from the same tokens `terminalTheme` uses, not written as literals:
  // decorations are painted into the same canvas no contrast gate can see, so
  // a hardcoded palette here would be the theming defect again in a place the
  // guard for it would not have looked.
  const styles = getComputedStyle(document.documentElement);
  const token = (name) => styles.getPropertyValue(name).trim();
  const options = {
    decorations: {
      matchOverviewRuler: token("--yellow"),
      activeMatchColorOverviewRuler: token("--red"),
      matchBackground: token("--term-selection"),
      activeMatchBackground: token("--yellow"),
    },
  };
  if (direction === "previous") state.searchAddon.findPrevious(needle, options);
  else state.searchAddon.findNext(needle, options);
}

/// Report the addon's own match count.
///
/// `resultCount` is -1 while the addon is still scanning a long buffer, and 0
/// when nothing matched. The two are different answers and the row says which:
/// showing "0 matches" during a scan is a wrong answer that arrives before the
/// right one.
function renderFindCount(results) {
  const element = $("terminal-find-count");
  if (!results || results.resultCount < 0) {
    element.textContent = t("find-searching");
    return;
  }
  if (results.resultCount === 0) {
    element.textContent = t("find-none");
    return;
  }
  element.textContent = t("find-position", {
    index: results.resultIndex + 1,
    total: results.resultCount,
  });
}

/// Prepend output the in-memory ring has already dropped.
///
/// The terminal is reset and rewritten rather than scrolled backwards: xterm has
/// no way to insert above existing content, and replaying history followed by
/// the ring is the only ordering that reads correctly. The live stream keeps
/// arriving on its own channel throughout.
async function loadOlderOutput() {
  const id = state.focused;
  if (!id || state.historyLoading) return;
  state.historyLoading = true;
  try {
    const generation = state.focusGeneration;
    const chunks = [];
    const channel = new Channel();
    channel.onmessage = (data) => chunks.push(data);
    await invoke("stream_scrollback_history", {
      id,
      maxBytes: HISTORY_REQUEST_BYTES,
      channel,
    });
    // A focus switch while the read was in flight would otherwise paint one
    // session's history into another session's pane.
    if (state.focused !== id || state.focusGeneration !== generation) return;
    const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
    if (!total) {
      showToast(t("history-empty"));
      return;
    }
    state.terminal?.reset();
    for (const chunk of chunks) writeTerminalBytes(chunk, id, generation);
    showToast(t("history-loaded", { bytes: Math.round(total / 1024) }), "success");
  } catch (error) {
    showToast(t("history-load-error", { error: String(error) }));
  } finally {
    state.historyLoading = false;
  }
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
    if (action === "queue") await openQueue(id);
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
      showToast(t("reply-sent"), "success");
    }
    if (action === "kill") {
      await invoke("kill_session", { id });
      showToast(t("stop-signal-sent"), "success");
    }
    if (action === "revive") {
      await invoke("revive_session", { id });
      showToast(t("resume-started"), "success");
    }
    if (action === "archive") {
      await invoke("archive_session", { id });
      showToast(t("archive-stopped"), "success");
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
    allowed_tools: [],
    disallowed_tools: [],
    settings: null,
    setting_sources: null,
    mcp_config: [],
    strict_mcp_config: false,
    plugin_dirs: [],
    plugin_urls: [],
    fallback_model: null,
    environment: { setup: null, teardown: null, port_base: 42000, port_count: 4 },
    worktree: false,
  };
}

/// One comma-separated field as a list, with empty entries dropped. An empty
/// entry would reach the agent as a bare flag with nothing after it.
function commaList(id) {
  return $(id)
    .value.split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
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
    worktree: $("worktree-input").checked,
    agent_home: $("agent-home-input").value.trim() || null,
    // Names only. The core reads each value from this process and refuses a
    // name that is unset, so an empty entry must not reach it as one.
    env_passthrough: $("env-passthrough-input")
      .value.split(",")
      .map((name) => name.trim())
      .filter(Boolean),
    // Claude-only on the versions this build maps. Sent only for Claude so a
    // Codex launch is not refused for a field the operator left behind when
    // switching agents — the core refuses these on Codex by design, and that
    // refusal should describe a choice, not a stale form.
    allowed_tools: agent === "claude" ? commaList("allowed-tools-input") : [],
    disallowed_tools: agent === "claude" ? commaList("disallowed-tools-input") : [],
    settings: agent === "claude" ? $("settings-input").value.trim() || null : null,
    setting_sources:
      agent === "claude" ? $("setting-sources-input").value.trim() || null : null,
    mcp_config: agent === "claude" ? commaList("mcp-config-input") : [],
    strict_mcp_config: agent === "claude" && $("strict-mcp-input").checked,
    plugin_dirs: agent === "claude" ? commaList("plugin-dirs-input") : [],
    plugin_urls: agent === "claude" ? commaList("plugin-urls-input") : [],
    fallback_model:
      agent === "claude" ? $("fallback-model-input").value.trim() || null : null,
    environment: {
      setup: $("setup-hook-input").value.trim() || null,
      teardown: $("teardown-hook-input").value.trim() || null,
      port_base: Number.isInteger(portBase) ? portBase : 42000,
      port_count: Number.isInteger(portCount) ? portCount : 4,
    },
  };
}

// A <select> silently discards a value it has no option for, so a preset or a
// resumed spec naming a permission mode this build does not model would come
// back as "" and launch with no mode at all. The core keeps such a value
// (Permission::Custom); this carries it into the list so it round-trips and the
// operator can see what is about to be launched. Previous carried-in options are
// dropped first so switching presets does not accumulate them.
function setPermissionValue(value) {
  const select = $("permission-input");
  for (const option of Array.from(select.options)) {
    if (option.dataset.passthrough === "true") option.remove();
  }
  const wanted = value ?? "ask";
  if (!Array.from(select.options).some((option) => option.value === wanted)) {
    const option = document.createElement("option");
    option.value = wanted;
    option.textContent = t("launcher-permission-custom", { mode: wanted });
    option.dataset.passthrough = "true";
    select.append(option);
  }
  select.value = wanted;
}

function writeSpec(spec) {
  clearFolderValidation();
  $("agent-input").value = spec.agent ?? "claude";
  $("name-input").value = spec.name ?? "";
  // A built-in preset names no folder: which configuration and which project
  // are separate choices, so applying "Plan first" must not retarget the
  // session to nowhere. Only a preset that actually carries a folder sets one.
  if (spec.cwd) $("cwd-input").value = spec.cwd;
  $("model-input").value = spec.model ?? "";
  $("effort-input").value = spec.effort ?? "";
  setPermissionValue(spec.permission);
  $("sandbox-input").value = spec.sandbox ?? "workspace-write";
  $("profile-input").value = spec.profile ?? "";
  $("resume-input").value = spec.resume?.kind ?? "new";
  $("resume-id-input").value = spec.resume?.id ?? "";
  $("budget-input").value = spec.max_budget_usd ?? "";
  $("search-input").checked = Boolean(spec.web_search);
  $("worktree-input").checked = Boolean(spec.worktree);
  $("agent-home-input").value = spec.agent_home ?? "";
  $("env-passthrough-input").value = (spec.env_passthrough ?? []).join(", ");
  $("allowed-tools-input").value = (spec.allowed_tools ?? []).join(", ");
  $("disallowed-tools-input").value = (spec.disallowed_tools ?? []).join(", ");
  $("settings-input").value = spec.settings ?? "";
  $("setting-sources-input").value = spec.setting_sources ?? "";
  $("mcp-config-input").value = (spec.mcp_config ?? []).join(", ");
  $("strict-mcp-input").checked = Boolean(spec.strict_mcp_config);
  $("plugin-dirs-input").value = (spec.plugin_dirs ?? []).join(", ");
  $("plugin-urls-input").value = (spec.plugin_urls ?? []).join(", ");
  $("fallback-model-input").value = spec.fallback_model ?? "";
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

function clearFolderValidation() {
  const input = $("cwd-input");
  input.removeAttribute("aria-invalid");
  input.setCustomValidity("");
  const message = $("cwd-error");
  message.hidden = true;
  message.textContent = "";
}

function showFolderValidation() {
  const input = $("cwd-input");
  const message = $("cwd-error");
  const text = t("launcher-folder-required");
  input.setAttribute("aria-invalid", "true");
  input.setCustomValidity(text);
  message.textContent = text;
  message.hidden = false;
  input.focus();
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
  // Choosing Claude used to silently rewrite a plan-mode selection to "ask".
  // It had been there unchanged since the first Tauri shell commit, with no
  // test and no recorded reason, and it rewrote two of this tool's own built-in
  // presets the moment the launcher synced its fields.
  //
  // Removed 2026-08-07 after verifying against the installed build rather than
  // the documentation: `claude --help` lists `plan` among the accepted
  // `--permission-mode` choices, and `claude --permission-mode plan --print`
  // runs and exits 0. `launch.rs` has always mapped Permission::Plan for both
  // agents, so the launcher was the only thing that disagreed.
  document.querySelectorAll(".resume-id-field").forEach((element) => element.classList.toggle("field-hidden", $("resume-input").value === "new" || $("resume-input").value === "last"));
}

function schedulePreview() {
  clearTimeout(state.previewTimer);
  const request = ++state.previewRequest;
  state.previewTimer = setTimeout(() => updatePreview(request), 180);
}

async function updatePreview(request) {
  const spec = readSpec();
  if (!spec.cwd) {
    $("preview-output").textContent = t("preview-folder");
    $("preview-state").textContent = t("preview-waiting");
    return;
  }
  $("preview-state").textContent = t("preview-resolving");
  try {
    const command = await invoke("preview_launch", invokeArgs(spec));
    if (request !== state.previewRequest) return;
    $("preview-output").textContent = command;
    $("preview-state").textContent = t("preview-exact");
  } catch (error) {
    if (request !== state.previewRequest) return;
    $("preview-output").textContent = String(error);
    $("preview-state").textContent = t("preview-refused");
  }
}

async function launchCurrentSpec() {
  const spec = readSpec();
  if (!spec.cwd) {
    showFolderValidation();
    return false;
  }
  clearFolderValidation();
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
    const selected = $("preset-select").value;
    // Built-ins are labelled, not silently mixed in: an operator who cannot see
    // which ones shipped with the app cannot tell why one of them refuses to be
    // overwritten.
    $("preset-select").innerHTML = `<option value="">${escapeHtml(t("button-presets"))}</option>${state.presets
      .map((preset) => {
        const label = preset.builtin ? `${preset.name} ${t("preset-builtin-mark")}` : preset.name;
        const title = preset.description ? ` — ${preset.description}` : "";
        return `<option value="${escapeHtml(preset.name)}" title="${escapeHtml(`${label}${title}`)}">${escapeHtml(label)}</option>`;
      })
      .join("")}`;
    if (state.presets.some((preset) => preset.name === selected)) $("preset-select").value = selected;
    $("delete-preset-button").disabled = !$("preset-select").value;
  } catch (error) {
    showToast(t("presets-load-error", { error: String(error) }));
  }
}

/**
 * Offer the launch configurations the chosen repository declares about itself.
 *
 * Re-read every time the folder changes rather than cached: the file is
 * versioned with the repository, so pulling a branch that changes it should
 * change what the launcher offers.
 *
 * A repository with no templates hides the control entirely — an empty dropdown
 * reads as "this project has none configured yet", which is a different and
 * more distracting claim than not mentioning it.
 */
async function loadProjectTemplates() {
  const field = document.querySelector(".project-template-field");
  const cwd = $("cwd-input").value.trim();
  state.templates = [];
  if (!cwd) {
    field.hidden = true;
    return;
  }
  try {
    state.templates = await invoke("list_templates", { cwd });
  } catch (error) {
    // Said out loud, never swallowed: launching now would apply the operator's
    // own defaults while they believe the project's were used.
    field.hidden = true;
    showToast(t("template-unreadable", { detail: String(error) }));
    return;
  }
  field.hidden = state.templates.length === 0;
  $("template-select").innerHTML = `<option value="">${escapeHtml(t("template-none"))}</option>${state.templates
    .map(
      (template, index) =>
        `<option value="${index}">${escapeHtml(template.name)}${
          template.description ? ` — ${escapeHtml(template.description)}` : ""
        }</option>`,
    )
    .join("")}`;
}

/**
 * Apply the chosen template to the form.
 *
 * The folder is deliberately not touched: it is the repository the template was
 * read from, which is the one choice the operator has already made.
 */
function applyProjectTemplate() {
  const index = Number.parseInt($("template-select").value, 10);
  const template = state.templates[index];
  if (!template) return;
  const cwd = $("cwd-input").value.trim();
  if (template.agent) $("agent-input").value = template.agent;
  if (template.model) $("model-input").value = template.model;
  if (template.effort) $("effort-input").value = template.effort;
  if (template.permission) setPermissionValue(template.permission);
  if (template.sandbox) $("sandbox-input").value = template.sandbox;
  if (template.profile) $("profile-input").value = template.profile;
  if (template.prompt) $("prompt-input").value = template.prompt;
  $("worktree-input").checked = Boolean(template.worktree);
  $("search-input").checked = Boolean(template.web_search);
  state.extraDirs = (template.add_dirs ?? []).map((dir) => `${cwd}/${dir}`);
  $("extra-dirs-input").value = state.extraDirs.join("; ");
  syncAgentFields();
  schedulePreview();
  showToast(t("template-applied", { name: template.name }), "success");
}

/**
 * Offer every repository under the registered roots as a launch target.
 *
 * Re-read rather than cached: the point of the list is being current, so a
 * repository cloned five minutes ago is launchable without telling the app.
 *
 * With no root registered the control is hidden entirely — an empty "Known
 * projects" dropdown is a question the operator has no way to answer. The
 * register button beside Browse is what they see instead.
 */
function renderProjectRoots() {
  const list = $("project-root-list");
  if (state.projectRootsError) {
    renderDataError(
      list,
      t("projects-roots-load-error", { error: state.projectRootsError }),
      "project-roots",
      loadProjectRoots,
    );
    return;
  }
  if (!state.projectRoots.length) {
    list.innerHTML = '<li class="rollup-total">' + escapeHtml(t("projects-roots-empty")) + "</li>";
    return;
  }
  list.innerHTML = state.projectRoots
    .map((root) => {
      const value = String(root);
      const label = t("projects-root-remove", { root: value });
      return '<li class="project-root-row"><code class="project-root-path" title="' + escapeHtml(value) + '">' + escapeHtml(value) + '</code><button type="button" class="button button-quiet" data-project-root-remove="' + escapeHtml(value) + '" title="' + escapeHtml(label) + '" aria-label="' + escapeHtml(label) + '">' + escapeHtml(t("button-remove")) + "</button></li>";
    })
    .join("");
}

async function loadProjectRoots() {
  try {
    state.projectRoots = await invoke("list_project_roots");
    state.projectRootsError = null;
  } catch (error) {
    state.projectRoots = [];
    state.projectRootsError = String(error);
  }
  renderProjectRoots();
}

async function refreshScannedProjects() {
  try {
    state.scannedProjects = await invoke("scan_projects");
    state.projectsError = null;
  } catch (error) {
    state.scannedProjects = [];
    state.projectsError = String(error);
  }
  renderProjects();
}

async function removeProjectRoot(path) {
  try {
    const removed = await invoke("remove_project_root", { path });
    await loadProjectRoots();
    await loadKnownProjects();
    if ($("projects-dialog").open) await refreshScannedProjects();
    showToast(
      removed ? t("projects-root-removed", { root: path }) : t("projects-root-not-found", { root: path }),
      removed ? "success" : "",
    );
  } catch (error) {
    showToast(String(error));
  }
}

async function loadKnownProjects() {
  const field = document.querySelector(".known-projects-field");
  try {
    state.projects = await invoke("list_projects");
  } catch (error) {
    state.projects = [];
    showToast(String(error));
  }
  field.hidden = state.projects.length === 0;
  $("register-root-empty-button").hidden = state.projects.length > 0;
  $("project-select").innerHTML = `<option value="">${escapeHtml(t("project-choose"))}</option>${state.projects
    .map(
      (project) =>
        `<option value="${escapeHtml(project.path)}" title="${escapeHtml(project.path)}">${escapeHtml(project.name)}</option>`,
    )
    .join("")}`;
}

/**
 * Register a folder that holds repositories.
 *
 * Reports how many projects it found. "Registered" alone leaves the operator
 * unable to tell a working root from one pointed at the wrong directory, and
 * the difference only shows up later as an empty dropdown.
 */
async function registerProjectRoot() {
  let root;
  try {
    root = await invoke("pick_folder");
  } catch (error) {
    showToast(String(error));
    return;
  }
  if (!root) return;
  try {
    await invoke("add_project_root", { path: root });
  } catch (error) {
    showToast(String(error));
    return;
  }
  await Promise.all([loadProjectRoots(), loadKnownProjects()]);
  if ($("projects-dialog").open) await refreshScannedProjects();
  const found = state.projects.filter((project) => project.root === root).length;
  showToast(
    found ? t("projects-root-added", { root, count: found }) : t("projects-none-found", { root }),
    found ? "success" : "",
  );
}

async function saveCurrentPreset() {
  const name = $("preset-name-input").value.trim();
  if (!name) {
    showToast(t("preset-name-required"));
    $("preset-name-input").focus();
    return;
  }
  try {
    await invoke("save_preset", {
      preset: { name, spec: readSpec(), configured_path: null, builtin: false, description: null },
    });
    await loadPresets();
    $("preset-name-input").value = "";
    showToast(t("preset-saved", { name }), "success");
  } catch (error) {
    showToast(String(error));
  }
}

async function deleteSelectedPreset() {
  const select = $("preset-select");
  const name = select.value;
  if (!name) return;
  try {
    const removed = await invoke("delete_preset", { name });
    await loadPresets();
    showToast(
      removed ? t("preset-deleted", { name }) : t("preset-not-found", { name }),
      removed ? "success" : "",
    );
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
  void loadKnownProjects();
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
  // The same renderer geometry must be sent once per session. A global
  // `cols x rows` signature would suppress the first resize after switching
  // from one session to another, leaving the new pty at its 120x40 default.
  const signature = `${state.focused}:${cols}x${rows}`;
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
  showToast(t("link-opened", { host: new URL(opened).host || opened }), "success");
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

/// The terminal's palette, read from the same custom properties every other
/// surface uses.
///
/// It used to be a literal here, which meant the one surface that fills most of
/// the window ignored `prefers-color-scheme` entirely: in light mode the
/// operator got a light panel framing a hard dark rectangle, and focusing a
/// session flipped the pane's apparent theme. The canvas is not DOM text, so no
/// contrast gate could see it.
function terminalTheme() {
  const styles = getComputedStyle(document.documentElement);
  const token = (name) => styles.getPropertyValue(name).trim();
  return {
    background: token("--term-bg"),
    foreground: token("--term-fg"),
    cursor: token("--term-cursor"),
    selectionBackground: token("--term-selection"),
    black: token("--term-black"),
    red: token("--red"),
    green: token("--green"),
    yellow: token("--yellow"),
    blue: token("--blue"),
    magenta: token("--mauve"),
    cyan: token("--teal"),
    white: token("--term-white"),
  };
}

/// Repaint the terminal when the OS theme changes under a running window.
///
/// The rest of the chrome is CSS and follows on its own; the canvas is painted
/// from values read once, so without this the pane keeps the palette it started
/// with and becomes the only surface out of step.
function followColorScheme() {
  const scheme = window.matchMedia?.("(prefers-color-scheme: dark)");
  scheme?.addEventListener?.("change", () => {
    if (!state.terminal) return;
    state.terminal.options.theme = terminalTheme();
  });
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
    theme: terminalTheme(),
  });
  followColorScheme();
  state.fitAddon = new FitAddon();
  state.terminal.loadAddon(state.fitAddon);
  // Find-in-pane. The addon reports its own match count through
  // `onDidChangeResults`, which is the number worth showing: a bare "found /
  // not found" makes the operator page through the pane to learn how much
  // there is.
  state.searchAddon = new SearchAddon();
  state.terminal.loadAddon(state.searchAddon);
  state.searchAddon.onDidChangeResults((results) => renderFindCount(results));
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
    default:
      break;
  }
}

function bindEvents() {
  $("new-session-button").addEventListener("click", openLauncher);
  $("empty-new-button").addEventListener("click", openLauncher);
  $("fleet-summary").addEventListener("click", (event) => {
    if (event.target?.closest?.("#fleet-spend")) openRollup();
  });
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
  $("update-open-releases").addEventListener("click", () => void openSessionLink(RELEASES_PAGE));
  $("filter-input").addEventListener("input", renderRows);
  $("agent-filter").addEventListener("change", (event) => {
    state.agentFilter = event.target.value;
    renderRows();
  });
  $("status-filter").addEventListener("change", (event) => {
    state.statusFilter = event.target.value;
    renderRows();
  });
  $("group-toggle").addEventListener("click", () => {
    // Cycles rather than opening a menu: four modes is fewer clicks this way,
    // and the button always states the current one.
    const next = (GROUP_MODES.indexOf(state.groupBy) + 1) % GROUP_MODES.length;
    state.groupBy = GROUP_MODES[next];
    renderRows();
  });
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
      if (id === "cwd-input") clearFolderValidation();
      if (id === "resume-input" || id === "model-input" || id === "effort-input") syncAgentFields();
      schedulePreview();
    });
    $(id).addEventListener("change", () => {
      if (id === "cwd-input") clearFolderValidation();
      if (id === "resume-input" || id === "model-input" || id === "effort-input") syncAgentFields();
      schedulePreview();
    });
  });
  $("pick-folder-button").addEventListener("click", async () => {
    let folder;
    try {
      folder = await invoke("pick_folder");
    } catch (error) {
      showToast(String(error));
      return;
    }
    if (folder) {
      $("cwd-input").value = folder;
      clearFolderValidation();
      schedulePreview();
      void loadProjectTemplates();
    }
  });
  $("cwd-input").addEventListener("change", () => void loadProjectTemplates());
  $("register-root-button").addEventListener("click", () => void registerProjectRoot());
  $("register-root-empty-button").addEventListener("click", () => void registerProjectRoot());
  $("project-root-add-button").addEventListener("click", () => void registerProjectRoot());
  $("project-root-list").addEventListener("click", (event) => {
    const button = event.target.closest("button[data-project-root-remove]");
    if (button) void removeProjectRoot(button.dataset.projectRootRemove);
  });
  $("project-select").addEventListener("change", () => {
    const path = $("project-select").value;
    if (!path) return;
    $("cwd-input").value = path;
    clearFolderValidation();
    schedulePreview();
    // The chosen project may declare its own templates; the folder changed
    // without the input's change event firing.
    void loadProjectTemplates();
  });
  $("template-select").addEventListener("change", () => applyProjectTemplate());
  $("pick-extra-button").addEventListener("click", async () => {
    let folders;
    try {
      folders = await invoke("pick_extra_dirs");
    } catch (error) {
      showToast(String(error));
      return;
    }
    if (folders?.length) {
      state.extraDirs = folders;
      $("extra-dirs-input").value = folders.join("; ");
      schedulePreview();
    }
  });
  $("save-preset-button").addEventListener("click", saveCurrentPreset);
  $("preset-select").addEventListener("change", () => {
    $("delete-preset-button").disabled = !$("preset-select").value;
  });
  $("delete-preset-button").addEventListener("click", () => void deleteSelectedPreset());
  $("launch-preset-button").addEventListener("click", loadSelectedPreset);
  $("restore-presets-button").addEventListener("click", async () => {
    try {
      const restored = await invoke("restore_builtin_presets");
      await loadPresets();
      showToast(
        restored ? t("presets-restored", { count: restored }) : t("presets-none-hidden"),
        restored ? "success" : "",
      );
    } catch (error) {
      showToast(String(error));
    }
  });
  $("cancel-launch-button").addEventListener("click", () => $("launcher-dialog").close());
  $("close-launcher-button").addEventListener("click", () => $("launcher-dialog").close());
  $("close-rollup-button").addEventListener("click", () => $("rollup-dialog").close());
  $("close-queue-button").addEventListener("click", () => $("queue-dialog").close());
  $("queue-add-button").addEventListener("click", () => void addQueuedPrompt());
  $("queue-pause-button").addEventListener("click", async () => {
    try {
      await invoke("pause_queue", { id: state.queueSession });
    } catch (error) {
      showToast(String(error));
    }
    await refreshQueue();
  });
  $("queue-resume-button").addEventListener("click", async () => {
    try {
      await invoke("resume_queue", { id: state.queueSession });
    } catch (error) {
      showToast(String(error));
    }
    await refreshQueue();
  });
  wireOverflowMenus($("app-menu-button").ownerDocument);
  $("explainer-toggle").addEventListener("click", () => openExplainer());
  $("settings-toggle").addEventListener("click", () => void openSettings());
  $("history-toggle").addEventListener("click", () => void openSessionHistory());
  $("close-history-button").addEventListener("click", () => $("history-dialog").close());
  $("fleet-search-toggle").addEventListener("click", () => {
    const dialog = $("search-dialog");
    if (!dialog.open) dialog.showModal();
    $("fleet-search-input").focus();
    $("fleet-search-input").select();
  });
  $("close-search-button").addEventListener("click", () => $("search-dialog").close());
  $("approvals-toggle").addEventListener("click", () => openApprovals());
  $("close-approvals-button").addEventListener("click", () => $("approvals-dialog").close());
  $("working-sets-toggle").addEventListener("click", () => void openWorkingSets());
  $("close-working-sets-button").addEventListener("click", () => $("working-sets-dialog").close());
  $("working-set-save").addEventListener("click", async () => {
    const name = $("working-set-name").value.trim();
    if (!name) {
      showToast(t("working-sets-needs-name"));
      return;
    }
    try {
      const count = await invoke("save_working_set", { name, groupBy: state.groupBy });
      $("working-set-name").value = "";
      showToast(t("working-sets-saved", { name, count }), "success");
      await refreshWorkingSets();
    } catch (error) {
      showToast(String(error));
    }
  });
  $("fleet-search-run").addEventListener("click", () => void runFleetSearch());
  $("fleet-search-input").addEventListener("keydown", (event) => {
    if (event.key === "Enter") void runFleetSearch();
  });
  $("save-settings-button").addEventListener("click", () => void saveSettings());
  $("close-settings-button").addEventListener("click", () => $("settings-dialog").close());
  $("empty-explainer-button").addEventListener("click", () => openExplainer());
  $("close-explainer-button").addEventListener("click", () => $("explainer-dialog").close());
  $("empty-root-button").addEventListener("click", () => void registerProjectRoot());
  $("projects-toggle").addEventListener("click", () => void openProjects());
  $("close-projects-button").addEventListener("click", () => $("projects-dialog").close());
  $("prompts-toggle").addEventListener("click", () => openPromptLibrary());
  $("close-prompt-button").addEventListener("click", () => $("prompt-dialog").close());
  $("prompt-new-button").addEventListener("click", newStoredPrompt);
  $("prompt-save-button").addEventListener("click", () => void saveStoredPrompt());
  $("stored-prompt-list").addEventListener("click", (event) => {
    const select = event.target.closest("button[data-prompt-select]");
    if (select) {
      selectStoredPrompt(select.dataset.promptSelect);
      return;
    }
    const remove = event.target.closest("button[data-prompt-delete]");
    if (remove) void deleteStoredPrompt(remove.dataset.promptDelete);
  });
  $("projects-open-only").addEventListener("change", () => renderProjects());
  $("work-start-button").addEventListener("click", () => void startWorkRun());
  $("work-pause-button").addEventListener("click", () => void setWorkRunPaused(true));
  $("work-resume-button").addEventListener("click", () => void setWorkRunPaused(false));
  $("work-clear-button").addEventListener("click", async () => {
    try {
      await invoke("clear_work_run");
    } catch (error) {
      showToast(String(error));
    }
    await refreshWorkRun();
  });
  $("broadcast-toggle").addEventListener("click", () => openBroadcast());
  $("broadcast-list").addEventListener("change", () => syncBroadcastSelection());
  $("cancel-broadcast-button").addEventListener("click", () => $("broadcast-dialog").close());
  $("send-broadcast-button").addEventListener("click", () => void sendBroadcast());
  // Launching costs tokens and writes to a real repository, so it is reachable only
  // from the launch button. The form never submits: implicit submission on Enter in
  // any field would otherwise spawn an agent the operator never asked for.
  $("launcher-form").addEventListener("submit", (event) => event.preventDefault());
  $("launch-button").addEventListener("click", () => void launchCurrentSpec());
  $("terminal-clear").addEventListener("click", () => state.terminal?.clear());
  $("terminal-history").addEventListener("click", () => void loadOlderOutput());
  $("terminal-find-toggle").addEventListener("click", () => toggleFind());
  $("terminal-find-close").addEventListener("click", () => toggleFind(false));
  $("terminal-find-input").addEventListener("input", () => runFind());
  $("terminal-find-next").addEventListener("click", () => runFind("next"));
  $("terminal-find-previous").addEventListener("click", () => runFind("previous"));
  $("terminal-resize").addEventListener("click", async () => {
    if (!state.focused) return;
    try {
      state.lastSentSize = null;
      fitTerminal();
      showToast(t("terminal-refitted", { cols: state.terminal.cols, rows: state.terminal.rows }), "success");
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
    showToast(t("event-stream-unavailable", { error: String(error) }));
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
