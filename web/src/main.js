import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { reconcileKeyedRows } from "./fleetRows.js";
import "./styles.css";

const WDIO_BUILD = import.meta.env.VITE_TERMINALAI_WDIO === "1";

if (WDIO_BUILD) {
  await import("@wdio/tauri-plugin");
}

const STATUS_ORDER = {
  "needs-approval": 8,
  "awaiting-input": 7,
  "needs-you": 6,
  working: 5,
  thinking: 4,
  idle: 3,
  starting: 2,
  queued: 1,
  unknown: 1,
  exited: 0,
};

const STATUS_META = {
  "needs-approval": { glyph: "⚠", label: "Needs approval", short: "approval", tone: "peach" },
  "awaiting-input": { glyph: "?", label: "Awaiting input", short: "input", tone: "yellow" },
  "needs-you": { glyph: "!", label: "Needs you", short: "you", tone: "peach" },
  working: { glyph: "◒", label: "Working", short: "working", tone: "yellow" },
  thinking: { glyph: "✦", label: "Thinking", short: "thinking", tone: "mauve" },
  idle: { glyph: "·", label: "Idle", short: "idle", tone: "surface2" },
  starting: { glyph: "…", label: "Starting", short: "starting", tone: "sapphire" },
  queued: { glyph: "⏳", label: "Queued", short: "queued", tone: "overlay0" },
  unknown: { glyph: "∅", label: "State unknown", short: "unknown", tone: "overlay0" },
  exited: { glyph: "×", label: "Exited", short: "exited", tone: "overlay0" },
};
const STATUS_KEYS = Object.keys(STATUS_META);
const PREFLIGHT_META = {
  ok: { glyph: "✓", label: "Ready", tone: "green" },
  warn: { glyph: "!", label: "Needs attention", tone: "peach" },
  error: { glyph: "×", label: "Unavailable", tone: "red" },
  unsupported: { glyph: "—", label: "Not applicable", tone: "overlay0" },
};
const RELEASES_ENDPOINT = "https://api.github.com/repos/SysAdminDoc/TerminalAI/releases/latest";
const FALLBACK_APP_VERSION = "0.1.0";

function lifecycleLabel(session) {
  if (session?.phase === "preparing") return "Preparing environment";
  if (session?.phase === "tearing-down") return "Tearing down environment";
  return STATUS_META[session?.status]?.label ?? session?.status ?? "Unknown";
}

const MODEL_SUGGESTIONS = {
  claude: ["opus", "sonnet", "haiku"],
  codex: ["gpt-5.1-codex", "gpt-5.1-codex-mini", "gpt-5.1"],
};

const state = {
  sessions: [],
  focused: null,
  presets: [],
  extraDirs: [],
  attentionOnly: false,
  wideMode: false,
  reviewMode: false,
  reviews: [],
  diagnosticsMode: false,
  appVersion: null,
  storeQuarantine: null,
  storeQuarantineDismissed: false,
  terminal: null,
  outputChannel: null,
  fitAddon: null,
  previewTimer: null,
  preflight: null,
  preflightMode: false,
  preflightLoading: false,
  preflightReason: null,
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

function writeTerminalBytes(payload, id = state.focused) {
  if (id === state.focused && state.terminal) state.terminal.write(terminalBytes(payload));
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
    ? `The unreadable session store was moved to ${path}. New sessions start empty.`
    : "";
}

function showAttentionToast(notification) {
  if (state.attentionToasts.has(notification.dedup_key)) return;
  const session = state.sessions.find((item) => item.id === notification.session_id);
  const meta = STATUS_META[notification.status] ?? STATUS_META["needs-you"];
  const toast = document.createElement("button");
  toast.type = "button";
  toast.className = "toast toast-attention toast-visible";
  toast.textContent = `${session?.name ?? notification.session_id} · ${meta.label} · ${folderLabel(notification.group_key)}`;
  toast.title = "Focus session";
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
  const seconds = Math.max(0, Math.floor((Date.now() - systemTimeMs(value)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

function toolProgress(value) {
  const completed = Number(value?.completed);
  const total = Number(value?.total);
  if (!Number.isInteger(completed) || !Number.isInteger(total) || completed < 0 || total <= 0) return "—";
  return `${Math.min(completed, total)}/${total}`;
}

function cost(value) {
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
  return String(value ?? "unknown")
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
    host.innerHTML = '<div class="diagnostics-empty">Focus a session to inspect its status evidence.</div>';
    return;
  }
  const history = Array.isArray(session.status_history) ? [...session.status_history].reverse() : [];
  const latest = history[0];
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const label = lifecycleLabel(session);
  const source = latest?.source ? diagnosticSource(latest.source) : "Unavailable";
  const timeline = history.length
    ? history.map((entry) => {
      const entryMeta = STATUS_META[entry.to] ?? STATUS_META.exited;
      const from = entry.from ? (STATUS_META[entry.from]?.label ?? entry.from) : "Session created";
      return '<li class="diagnostic-event"><span class="diagnostic-event-glyph tone-' + entryMeta.tone + '" aria-hidden="true">' + entryMeta.glyph + '</span><div class="diagnostic-event-body"><div><b>' + escapeHtml(entryMeta.label) + '</b><span>from ' + escapeHtml(from) + '</span></div><small>' + escapeHtml(diagnosticSource(entry.source)) + ' · ' + escapeHtml(diagnosticTime(entry.at)) + '</small>' + (entry.detail ? '<p>' + escapeHtml(entry.detail) + '</p>' : '') + '</div></li>';
    }).join("")
    : '<li class="diagnostics-empty">No transition history was persisted for this session.</li>';
  host.innerHTML = '<div class="diagnostics-heading"><div><span class="eyebrow">WHY THIS STATE</span><h2>' + escapeHtml(session.name) + '</h2><p>' + escapeHtml(session.cwd) + '</p></div><div class="diagnostics-heading-actions"><button type="button" class="button button-quiet" data-diagnostics-action="preflight">Preflight checks</button><span class="status-glyph tone-' + meta.tone + '" title="' + escapeHtml(label) + '" aria-hidden="true">' + meta.glyph + '</span></div></div>' +
    '<div class="diagnostics-current"><span>Current status</span><b>' + escapeHtml(label) + '</b><span>for ' + escapeHtml(dwell(session.status_since)) + ' · source ' + escapeHtml(source) + '</span></div>' +
    '<ol class="diagnostics-timeline">' + timeline + '</ol>';
}

function syncDiagnosticsVisibility() {
  const active = state.diagnosticsMode;
  $("terminal-host").classList.toggle("view-hidden", active);
  $("diagnostics-host").classList.toggle("view-hidden", !active);
  $("diagnostics-toggle").setAttribute("aria-pressed", String(active));
  $("diagnostics-toggle").classList.toggle("row-action-active", active);
  $("diagnostics-toggle").textContent = active ? "▣" : "?";
  if (active) renderDiagnostics();
}

function setDiagnosticsMode(active) {
  state.diagnosticsMode = active;
  syncDiagnosticsVisibility();
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
    ? "Checking local dependencies…"
    : state.preflightReason
      ? state.preflightReason
      : attention
        ? `${attention} check${attention === 1 ? "" : "s"} need attention before the fleet can be trusted.`
        : "All detected control-plane dependencies are ready.";
  $("preflight-list").innerHTML = checks.map((check) => {
    const meta = PREFLIGHT_META[check.state] ?? PREFLIGHT_META.error;
    const detail = check.detail ? `<small>${escapeHtml(check.detail)}</small>` : "";
    const fixLabel = check.can_fix ? "Fix" : "Fix unavailable";
    return `<article class="preflight-row" role="listitem"><span class="status-glyph tone-${meta.tone}" title="${meta.label}" aria-hidden="true">${meta.glyph}</span><div class="preflight-copy"><div><b>${escapeHtml(check.label)}</b><span>${escapeHtml(meta.label)}</span></div><strong>${escapeHtml(check.detected)}</strong>${detail}</div><div class="preflight-actions"><button type="button" class="button button-secondary" data-preflight-action="fix" data-preflight-id="${escapeHtml(check.id)}"${check.can_fix ? "" : " disabled"} aria-label="${fixLabel} ${escapeHtml(check.label)}">${fixLabel}</button><button type="button" class="button button-quiet" data-preflight-action="recheck" data-preflight-id="${escapeHtml(check.id)}" aria-label="Recheck ${escapeHtml(check.label)}">Recheck</button></div></article>`;
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
  $("review-toggle").textContent = state.reviewMode && !state.preflightMode ? "Fleet" : "Review";
}

function renderReview() {
  const entries = Array.isArray(state.reviews) ? state.reviews : [];
  const pending = entries.filter((entry) => !entry.reviewed).length;
  const conflicts = entries.filter((entry) => (entry.conflicts?.length ?? 0) > 0 || reviewNumber(entry.conflict_markers) > 0).length;
  const timedOut = entries.filter((entry) => entry.timed_out === true).length;
  $("review-summary").textContent = entries.length + " session" + (entries.length === 1 ? "" : "s") + " · " + pending + " pending · " + conflicts + " with conflicts" + (timedOut ? " · " + timedOut + " timed out" : "");
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
  const status = entry.timed_out ? "Timed out" : (entry.reviewed ? "Reviewed" : "Pending");
  const conflictDetails = conflicts.length
    ? "<ul>" + conflicts.map((path) => "<li><code>" + escapeHtml(path) + "</code></li>").join("") + "</ul>"
    : "";
  const conflictMarkup = conflicts.length || markers
    ? '<div class="review-conflict" role="alert"><strong>Conflict markers surfaced</strong><span>' + conflicts.length + " conflicted file" + (conflicts.length === 1 ? "" : "s") + (markers ? " · " + markers + " marker lines" : "") + "</span>" + conflictDetails + "</div>"
    : "";
  const errorMarkup = entry.error ? '<div class="review-error" role="alert">' + escapeHtml(entry.error) + "</div>" : "";
  const diffMarkup = entry.diff
    ? '<details class="review-diff" ' + (conflicts.length || markers ? "open" : "") + "><summary>Show diff" + (entry.diff_truncated ? " · truncated" : "") + "</summary><pre>" + escapeHtml(entry.diff) + "</pre></details>"
    : '<div class="review-no-diff">No textual diff was returned.</div>';
  const actionMarkup = entry.reviewed
    ? '<span class="reviewed-label">✓ Reviewed</span>'
    : entry.error
      ? ""
      : '<button type="button" class="button button-secondary review-mark" data-review-action="mark-reviewed" data-review-id="' + escapeHtml(entry.session_id) + '">Mark reviewed</button>';
  return '<article class="review-entry' + (entry.reviewed ? " review-entry-reviewed" : "") + (entry.timed_out ? " review-entry-timeout" : "") + '" role="listitem">' +
    '<div class="review-entry-heading"><div><h3>' + escapeHtml(entry.name) + '</h3><div class="review-repo"><span>' + escapeHtml(folderLabel(entry.cwd)) + '</span><span>' + escapeHtml(agent) + '</span><code>' + escapeHtml(entry.session_id) + '</code></div></div><div class="review-entry-action">' + actionMarkup + "</div></div>" +
    '<div class="review-metrics"><span><b>' + files + "</b> file" + (files === 1 ? "" : "s") + '</span><span class="review-additions">+' + additions + '</span><span class="review-deletions">−' + deletions + '</span><span>cost ' + reviewCost + '</span><span class="review-state">' + status + "</span></div>" +
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
  const live = state.sessions.filter((session) => !["exited", "queued"].includes(session.status)).length;
  const queued = state.sessions.filter((session) => session.status === "queued").length;
  const needsYou = state.sessions.filter((session) => ["needs-you", "needs-approval", "awaiting-input"].includes(session.status)).length;
  const working = state.sessions.filter((session) => ["working", "thinking"].includes(session.status)).length;
  const spend = state.sessions.reduce((total, session) => total + (Number(session.cost_usd) || 0), 0);
  const maxLive = state.admission.max_live_sessions ?? 3;
  $("fleet-summary").innerHTML = `<span class="summary-item"><b>${live}/${maxLive}</b> live</span><span class="summary-separator">/</span><span class="summary-item"><b>${queued}</b> queued</span><span class="summary-separator">/</span><span class="summary-item summary-attention"><b>${needsYou}</b> needs you</span><span class="summary-separator">/</span><span class="summary-item"><b>${working}</b> active</span><span class="summary-separator">/</span><span class="summary-item"><b>$${spend.toFixed(2)}</b> spent</span>`;
  const droppedEvents = Number(state.admission.dropped_events) || 0;
  $("fleet-count").textContent = droppedEvents
    ? `${state.sessions.length} tracked · ${droppedEvents} event drops`
    : `${state.sessions.length} tracked`;
  const counts = Object.fromEntries(STATUS_KEYS.map((status) => [status, 0]));
  for (const session of state.sessions) {
    if (session.status in counts) counts[session.status] += 1;
  }
  $("fleet-state-strip").innerHTML = STATUS_KEYS.map((status) => {
    const meta = STATUS_META[status];
    return `<span class="state-chip tone-${meta.tone}" role="listitem" title="${escapeHtml(meta.label)}: ${counts[status]}" aria-label="${escapeHtml(meta.label)}: ${counts[status]}"><span class="state-chip-glyph" aria-hidden="true">${meta.glyph}</span><b>${counts[status]}</b><span>${escapeHtml(meta.short)}</span></span>`;
  }).join("");
}

function renderRows() {
  syncReviewVisibility();
  renderSummary();
  const filter = $("filter-input").value.trim().toLowerCase();
  const desiredSessions = sortedSessions().filter((session) => {
    if (state.attentionOnly && !isAttention(session)) return false;
    if (!filter) return true;
    return [session.name, session.cwd, folderLabel(session.cwd), session.branch, session.agent, session.model, session.status, session.phase, lifecycleLabel(session), session.last_line, toolProgress(session.tool_progress), session.restarts, ports(session.ports)]
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
  const lastLine = session.last_line || "No output yet";
  const pinLabel = session.pinned ? "Unpin" : "Pin";
  const sessionLabel = `${session.name}, ${label}, ${repo}, ${branch}, progress ${progress}, ${restartCount} restarts, ports ${ports(session.ports)}`;

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
    dot.title = "Unread attention";
    row.querySelector(".row-name").append(dot);
  } else if (!unread && unreadDot) {
    unreadDot.remove();
  }
  row.querySelector(".row-repo").textContent = repo;
  row.querySelector(".row-branch").textContent = branch;
  row.querySelector(".row-status-label").textContent = label;
  const portBadge = row.querySelector(".row-ports");
  portBadge.textContent = "ports " + ports(session.ports);

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
  focus.setAttribute("aria-label", `Focus ${session.name} terminal`);
  const revive = row.querySelector('[data-action="revive"]');
  revive.hidden = !(session.status === "exited" && session.resume_id);
  revive.title = `Revive ${session.name} with native resume`;
  revive.setAttribute("aria-label", `Revive ${session.name} with native resume`);
  const archive = row.querySelector('[data-action="archive"]');
  archive.hidden = session.status !== "exited";
  archive.setAttribute("aria-label", `Archive ${session.name}`);
  const stop = row.querySelector('[data-action="kill"]');
  stop.hidden = session.status === "exited";
  const stopLabel = session.status === "queued" ? "Cancel queued session" : `Stop ${session.name}`;
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
  replyInput.setAttribute("aria-label", `Reply to ${session.name}`);
  row.querySelector(".row-reply-send").setAttribute("aria-label", `Send reply to ${session.name}`);
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
    ? `${entries[0].name} needs you: ${entries[0].label}.`
    : `${entries.length} sessions need you: ${entries.map((entry) => entry.name).join(", ")}.`;
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
  const lastLine = session.last_line || "No output yet";
  const pinLabel = session.pinned ? "Unpin" : "Pin";
  const reviveHidden = session.status === "exited" && session.resume_id ? "" : " hidden";
  const archiveHidden = session.status === "exited" ? "" : " hidden";
  const stopHidden = session.status === "exited" ? " hidden" : "";
  const stopLabel = session.status === "queued" ? "Cancel queued session" : `Stop ${session.name}`;
  const replyHidden = isAttention(session) ? "" : " hidden";
  const wideHidden = state.wideMode ? "" : " hidden";
  const portsLabel = ports(session.ports);
  return `<article class="fleet-row${active}${unread}" data-id="${escapeHtml(session.id)}" role="option" tabindex="-1" aria-posinset="1" aria-setsize="1" aria-selected="false" aria-keyshortcuts="Enter Space ArrowUp ArrowDown Home End" aria-label="${escapeHtml(`${session.name}, ${label}, ${repo}, ${branch}, progress ${progress}, ${restartCount} restarts, ports ${portsLabel}`)}">
    <div class="row-identity"><span class="status-glyph tone-${meta.tone}" title="${escapeHtml(label)}" aria-hidden="true">${meta.glyph}</span><div class="row-name-wrap"><div class="row-name"><span class="row-name-text">${escapeHtml(session.name)}</span>${session.unread ? '<span class="unread-dot" title="Unread attention"></span>' : ""}</div><div class="row-folder"><span class="row-repo" title="Repository">${escapeHtml(repo)}</span><span class="row-branch" title="Branch">${escapeHtml(branch)}</span><span class="row-status-label">${escapeHtml(label)}</span><span class="row-ports" title="Allocated ports">ports ${escapeHtml(portsLabel)}</span></div></div></div>
    <div class="row-metrics"><span class="agent-badge agent-${session.agent}" title="${session.agent === "codex" ? "Codex" : "Claude Code"}" aria-label="${session.agent === "codex" ? "Codex" : "Claude Code"}">${agentLabel}</span><span class="row-progress" title="Tool progress"><small>PROG</small><b>${escapeHtml(progress)}</b></span><span class="row-restarts" title="Restart count">↻ ${restartCount}</span></div>
    <div class="row-dwell"><span>${dwell(session.status_since)}</span><small class="row-last-line" title="${escapeHtml(lastLine)}">${escapeHtml(lastLine)}</small></div>
    <div class="row-actions"><button type="button" data-action="pin" class="row-action ${session.pinned ? "row-action-active" : ""}" title="${pinLabel}" aria-label="${pinLabel} ${escapeHtml(session.name)}">${session.pinned ? "◆" : "◇"}</button><button type="button" data-action="focus" class="row-action" title="Focus terminal" aria-label="Focus ${escapeHtml(session.name)} terminal">↗</button><button type="button" data-action="revive" class="row-action" title="Revive ${escapeHtml(session.name)} with native resume" aria-label="Revive ${escapeHtml(session.name)} with native resume"${reviveHidden}>↻</button><button type="button" data-action="archive" class="row-action" title="Archive stopped session" aria-label="Archive ${escapeHtml(session.name)}"${archiveHidden}>▣</button><button type="button" data-action="kill" class="row-action row-action-danger" title="${escapeHtml(stopLabel)}" aria-label="${escapeHtml(stopLabel)}"${stopHidden}>×</button></div>
    <div class="row-wide-meta"${wideHidden}><span><small>MODEL</small><b data-row-model>${escapeHtml(model)}</b></span><span><small>EFFORT</small><b data-row-effort>${escapeHtml(effort)}</b></span><span><small>COST</small><b data-row-cost>${escapeHtml(cost(session.cost_usd))}</b></span></div>
    <div class="row-reply"${replyHidden}><input data-reply type="text" maxlength="500" placeholder="Reply without opening terminal" aria-label="Reply to ${escapeHtml(session.name)}" /><button type="button" data-action="reply" class="row-reply-send" title="Send reply" aria-label="Send reply to ${escapeHtml(session.name)}">↵</button></div>
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
}

function updateTerminalHeader() {
  renderTerminalPlaceholder();
  const session = state.sessions.find((item) => item.id === state.focused);
  if (!session) {
    $("terminal-name").textContent = "No focused session";
    $("terminal-path").textContent = "";
    $("terminal-status").textContent = "Waiting for a session";
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

function resetTerminal(status = "Waiting for a session") {
  if (state.terminal) state.terminal.reset();
  $("terminal-status").textContent = status;
  updateTerminalHeader();
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
  state.terminal?.reset();
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
  const channel = new Channel();
  channel.onmessage = (data) => writeTerminalBytes(data, id);
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
    effort: "high",
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
    effort: $("effort-input").value,
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
  $("effort-input").value = spec.effort ?? "high";
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

function syncAgentFields() {
  const codex = $("agent-input").value === "codex";
  document.querySelectorAll(".codex-only").forEach((element) => element.classList.toggle("field-hidden", !codex));
  document.querySelectorAll(".claude-only").forEach((element) => element.classList.toggle("field-hidden", codex));
  const suggestions = $("model-suggestions");
  suggestions.innerHTML = (MODEL_SUGGESTIONS[codex ? "codex" : "claude"] ?? []).map((model) => `<option value="${model}"></option>`).join("");
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
}

function openLauncher() {
  writeSpec(defaultSpec());
  $("launcher-dialog").showModal();
  $("cwd-input").focus();
}

function setupTerminal() {
  state.terminal = new Terminal({
    allowProposedApi: false,
    convertEol: false,
    cursorBlink: true,
    cursorStyle: "bar",
    fontFamily: "'Cascadia Code', 'SFMono-Regular', Consolas, monospace",
    fontSize: 13,
    lineHeight: 1.25,
    scrollback: 2000,
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
  state.terminal.open($("terminal-host"));
  state.terminal.resize(120, 40);
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
  $("refresh-button").addEventListener("click", loadSnapshot);
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
  });
  $("diagnostics-toggle").addEventListener("click", () => setDiagnosticsMode(!state.diagnosticsMode));
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
    schedulePreview();
  });
  ["cwd-input", "model-input", "name-input", "effort-input", "permission-input", "sandbox-input", "profile-input", "resume-input", "resume-id-input", "budget-input", "port-base-input", "port-count-input", "setup-hook-input", "teardown-hook-input", "prompt-input", "search-input"].forEach((id) => {
    $(id).addEventListener("input", () => {
      if (id === "resume-input") syncAgentFields();
      schedulePreview();
    });
    $(id).addEventListener("change", () => {
      if (id === "resume-input") syncAgentFields();
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
      await invoke("resize_session", { id: state.focused, rows: 40, cols: 120 });
      state.terminal?.resize(120, 40);
      showToast("Terminal reset to canonical 120 × 40 grid", "success");
    } catch (error) {
      showToast(String(error));
    }
  });
}

async function start() {
  setupTerminal();
  bindEvents();
  syncAgentFields();
  try {
    await listen("terminalai:event", ({ payload }) => handleDaemonEvent(payload));
  } catch (error) {
    showToast(`Event stream unavailable: ${error}`);
  }
  await loadPreflight();
  await Promise.all([loadSnapshot(), loadPresets()]);
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
