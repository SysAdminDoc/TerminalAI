import { Channel, invoke as tauriInvoke } from "@tauri-apps/api/core";
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
import {
  createSessionStatus,
  STATUS_KEYS,
  STATUS_META,
  STATUS_ORDER,
} from "./sessionStatus.js";
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
import { createTerminalPane } from "./terminalPane.js";
import { createWorkRunPanel } from "./workRunPanel.js";
import { renderWindowShares } from "./quotaWindow.js";
import { createLauncher } from "./launcher.js";
import { createQueuePanel } from "./queuePanel.js";
import { createWorkspacePages } from "./workspacePages.js";
import { createTerminalHistory } from "./terminalHistory.js";
import { createOperationalPanels } from "./operationalPanels.js";
import { createDaemonEvents } from "./daemonEvents.js";
import { createEventBindings } from "./eventBindings.js";
import {
  createFirstRunDemoSessions,
  demoStatusCount,
  isFirstRunDemoSession,
  readFirstRunProgress,
  saveFirstRunProgress,
} from "./firstRun.js";

const WDIO_BUILD = import.meta.env.VITE_TERMINALAI_WDIO === "1";

if (WDIO_BUILD) {
  await import("@wdio/tauri-plugin");
}

/// Every command this window sends to the backend.
///
/// Normally the bundled `@tauri-apps/api` binding. Under the end-to-end build,
/// a command the harness has mocked is answered by the mock instead.
///
/// This indirection is why that harness spent a release looking like it worked
/// and did not. WebdriverIO's Tauri plugin registers mocks in
/// `window.__wdio_mocks__` and *tries* to intercept `window.__TAURI__.core.invoke`
/// by redefining it — which fails silently on this WebView2 and leaves the
/// global pointing at the real backend. Probed inside a live run on 2026-08-08,
/// `window.__TAURI__.core.invoke` was still
/// `async (e, n = {}, t) => window.__TAURI_INTERNALS__.invoke(e, n, t)` with
/// eight mocks registered beside it. So the plugin only routes mocks inside its
/// own `browser.tauri.execute` wrapper; the application never saw one, every
/// `fleet_snapshot` reached a daemon the wdio build deliberately does not have,
/// and the window sat in its daemon-unavailable state while the mocks recorded
/// zero calls.
///
/// Reading the map per call rather than binding once is deliberate: the spec
/// registers mocks after the window has loaded and adds more between steps.
/// Anything unmocked still reaches the real backend, so this cannot quietly
/// turn a missing mock into a passing assertion.
const invoke = WDIO_BUILD
  ? (command, args) => {
      const mock = window.__wdio_mocks__?.[command];
      return mock ? Promise.resolve(mock(args)) : tauriInvoke(command, args);
    }
  : tauriInvoke;

const {
  lastActivity,
  lifecycleDetail,
  lifecycleLabel,
  lifecycleTone,
  metaLabel,
  statusLabel,
} = createSessionStatus({ t, rateLimitedLabel });
const PREFLIGHT_META = {
  ok: { glyph: "✓", label: "preflight-ready", tone: "green" },
  warn: { glyph: "!", label: "preflight-needs-attention", tone: "peach" },
  error: { glyph: "×", label: "preflight-unavailable", tone: "red" },
  blocked: { glyph: "⊘", label: "preflight-blocked", tone: "red" },
  unsupported: { glyph: "—", label: "preflight-not-applicable", tone: "overlay0" },
};
const FALLBACK_APP_VERSION = __APP_VERSION__;

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
  workSchedule: null,
  reviewError: null,
  firstRunProgress: readFirstRunProgress(),
  demoMode: false,
  demoPrevious: null,
  announcementQueue: new Map(),
  announcementTimer: null,
  orderFreeze: null,
  attentionToasts: new Map(),
  admission: { max_live_sessions: 3, live_sessions: 0, queued_sessions: 0, aggregate_cost_usd: 0, dropped_events: 0 },
};

const $ = (id) => document.getElementById(id);

const RAIL_DIALOG_PAGES = Object.freeze({
  "projects-dialog": "projects",
  "prompt-dialog": "prompts",
  "broadcast-dialog": "broadcast",
  "approvals-dialog": "approvals",
  "search-dialog": "search",
  "working-sets-dialog": "working-sets",
  "history-dialog": "history",
  "settings-dialog": "settings",
  "explainer-dialog": "explainer",
});

function syncRailPage(page) {
  for (const item of document.querySelectorAll(".rail-item[data-rail-page]")) {
    const active = item.dataset.railPage === page;
    item.classList.toggle("rail-item-active", active);
    if (active) item.setAttribute("aria-current", "page");
    else item.removeAttribute("aria-current");
  }
}

function renderFirstRunGuide() {
  const checklist = $("first-run-checklist");
  if (!checklist) return;
  const entries = Array.from(checklist.querySelectorAll("[data-first-run-step]"));
  const done = entries.filter((entry) => state.firstRunProgress[entry.dataset.firstRunStep]).length;
  $("first-run-progress").textContent = t("first-run-progress", {
    done,
    total: entries.length,
  });
  for (const entry of entries) {
    const complete = state.firstRunProgress[entry.dataset.firstRunStep] === true;
    entry.dataset.complete = String(complete);
    entry.querySelector(".first-run-step-state").textContent = t(
      complete ? "first-run-step-done" : "first-run-step-next",
    );
  }
}

function markFirstRunStep(step) {
  if (state.firstRunProgress[step] === true) return;
  state.firstRunProgress = saveFirstRunProgress({
    ...state.firstRunProgress,
    [step]: true,
  });
  renderFirstRunGuide();
}

function closeWorkspacePages() {
  for (const dialog of document.querySelectorAll("dialog.workspace-page[open]")) dialog.close();
}

/// Menu destinations are pages inside the persistent shell. They still use a
/// dialog element for the existing focus and accessibility contracts, but open
/// non-modally so the rail and top-level controls remain available.
function openWorkspacePage(dialog) {
  if (!dialog) return;
  for (const other of document.querySelectorAll("dialog.workspace-page[open]")) {
    if (other !== dialog) other.close();
  }
  if (state.preflightMode) setPreflightMode(false);
  if (!dialog.open) dialog.show();
  syncRailPage(dialog.dataset.workspacePage ?? RAIL_DIALOG_PAGES[dialog.id] ?? "fleet");
}

/// Keep the visual navigation spine in step with the workspace pages. The
/// overflow menu remains a compatibility path for keyboard users and tests;
/// the rail is the persistent route through the same handlers.
function wireRailNavigation() {
  const items = [...document.querySelectorAll(".rail-item[data-rail-page]")];
  if (!items.length) return;
  for (const item of items) {
    item.addEventListener("click", () => {
      const page = item.dataset.railPage;
      syncRailPage(page);
      const target = item.dataset.railTarget;
      if (target) $(target)?.click();
      else {
        closeWorkspacePages();
        if (state.preflightMode) setPreflightMode(false);
      }
    });
  }
  const syncFromDialogs = () => {
    if (state.preflightMode) {
      syncRailPage("preflight");
      return;
    }
    const open = [...document.querySelectorAll("dialog.workspace-page[open]")].at(-1);
    syncRailPage(open ? RAIL_DIALOG_PAGES[open.id] ?? "fleet" : "fleet");
  };
  if (typeof MutationObserver === "function") {
    const observer = new MutationObserver(syncFromDialogs);
    for (const dialog of document.querySelectorAll("dialog")) {
      observer.observe(dialog, { attributes: true, attributeFilter: ["open"] });
    }
  }
  syncFromDialogs();
}

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

/// Render into a dialog, and put the failure in that dialog if it throws.
///
/// Four of these surfaces render from state the window already holds, so they
/// have no loading state and correctly never had one — but they had no error
/// state either, and a renderer that throws leaves an open dialog with an empty
/// body and nothing said. The operator's read of that is "it is still loading",
/// which it is not and never will be.
///
/// Deliberately not a loading state as well. Adding one where the data is
/// already in memory would show a spinner that is never true.
function renderGuarded(container, message, action, retry, render) {
  try {
    render();
  } catch (error) {
    // Logged as well as shown: the message on screen is for the operator, and
    // the stack is for whoever has to find out why.
    console.error(`${action} failed to render`, error);
    renderDataError(container, `${message} ${error}`, action, retry);
  }
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

const launcherPanel = createLauncher({
  $,
  document,
  state,
  invoke,
  invokeArgs,
  showToast,
  t,
  escapeHtml,
  renderDataError,
  renderProjects: (...args) => workspacePages.renderProjects(...args),
  onFirstRunStep: markFirstRunStep,
});
const {
  bindEvents: bindLauncherEvents,
  openLauncher,
  clearFolderValidation,
  syncAgentFields,
  loadAgentCapabilities,
  schedulePreview,
  loadProjectTemplates,
  applyProjectTemplate,
  loadProjectRoots,
  refreshScannedProjects,
  removeProjectRoot,
  loadKnownProjects,
  registerProjectRoot,
  saveCurrentPreset,
  deleteSelectedPreset,
  loadSelectedPreset,
  loadPresets,
  launchCurrentSpec,
} = launcherPanel;

const queuePanel = createQueuePanel({
  $,
  state,
  invoke,
  showToast,
  t,
  escapeHtml,
  renderDataError,
});
const {
  bindEvents: bindQueueEvents,
  queueGlyph,
  queueTitle,
  openQueue,
  refreshQueue,
  addQueuedPrompt,
} = queuePanel;

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

function updateLimitUsageCard(name, used, limit, format = (value) => String(Math.round(value))) {
  const value = Number(used) || 0;
  const ceiling = Number(limit);
  const bounded = Number.isFinite(ceiling) && ceiling > 0;
  $(`settings-${name}-usage`).textContent = `${format(value)} / ${bounded ? format(ceiling) : "∞"}`;
  const progress = $(`settings-${name}-progress`);
  progress.max = bounded ? ceiling : Math.max(value, 1);
  progress.value = Math.min(value, progress.max);
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
    const live = Number(state.admission?.live_sessions) || 0;
    const spend = Number(state.admission?.spend_window_usd ?? state.admission?.aggregate_cost_usd) || 0;
    const memoryMb = state.sessions.reduce((total, session) => total + (Number(session.memory_bytes) || 0), 0) / 1048576;
    updateLimitUsageCard("live", live, settings.max_live_sessions);
    updateLimitUsageCard("spend", spend, settings.spend_ceiling_usd, (value) => formatCost(value));
    updateLimitUsageCard("memory", memoryMb, settings.memory_budget_mb, (value) => `${Math.round(value)} MB`);
    updateLimitUsageCard("process", 0, settings.max_processes_per_session);
    $("settings-process-usage").textContent = settings.max_processes_per_session == null
      ? "∞"
      : `≤ ${settings.max_processes_per_session}`;
    $("settings-error").hidden = true;
    openWorkspacePage($("settings-dialog"));
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

/// What the cost is being measured against, when it is being measured against
/// anything. A session launched with no cap says so rather than borrowing the
/// wording of one that simply has not reached its cap yet.
function costTitle(session) {
  const budget = Number(session?.budget_usd);
  if (!Number.isFinite(budget) || budget < 0) return t("cost-explained");
  const key = session?.budget_exhausted ? "cost-budget-spent" : "cost-budget-of";
  // The same formatter the figure itself uses. Two spellings of money on one
  // row is how "$5" ends up sitting next to "$5.00" in the same sentence.
  return t(key, { budget: formatCost(budget) });
}

/// What the memory figure covers.
///
/// The process count is the point: since agent teams, a row can be a lead plus
/// several separate agent instances inside one job, and the cap the reading is
/// compared against applies to all of them. A domain that owns no job reports
/// no count, and this says that rather than implying a tree of one.
function memoryTitle(session) {
  if (session?.memory_limited) return t("memory-limited-explained");
  const processes = Number(session?.memory_processes);
  if (!Number.isInteger(processes) || processes < 1) return t("memory-unscoped-explained");
  return t("memory-explained", { processes });
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

// Diagnostics, daemon logs, and preflight checks live in `operationalPanels.js`.
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
    // `data-status` names the key, not the label. The browser audit reads it to
    // discover every status the fleet models and builds its fixture from that,
    // so a status added here is contrast-checked without anyone updating the
    // gate — a gate whose coverage is a hand-written list certifies whatever is
    // missing from it.
    return '<span class="state-chip tone-' + escapeHtml(meta.tone) + '" data-status="' + escapeHtml(status) + '" role="listitem" title="' + escapeHtml(metaLabel(meta)) + ': ' + escapeHtml(counts[status]) + '" aria-label="' + escapeHtml(metaLabel(meta)) + ': ' + escapeHtml(counts[status]) + '"><span class="state-chip-glyph" aria-hidden="true">' + meta.glyph + '</span><b>' + counts[status] + '</b><span>' + escapeHtml(t(meta.short)) + "</span></span>";
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
  $("demo-mode-banner").hidden = !state.demoMode;
  renderFirstRunGuide();
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
      if (state.demoMode) {
        if (button.dataset.action === "focus") void focusSession(row.dataset.id);
        else if (button.dataset.action === "pin") {
          const session = state.sessions.find((item) => item.id === row.dataset.id);
          if (session) session.pinned = !session.pinned;
          renderRows();
        } else showToast(t("demo-read-only"), "success");
        return;
      }
      rowAction(button.dataset.action, row.dataset.id, row);
    });
  }
  const reply = row.querySelector("input[data-reply]");
  reply?.addEventListener("click", (event) => event.stopPropagation());
  reply?.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    if (state.demoMode) showToast(t("demo-read-only"), "success");
    else rowAction("reply", row.dataset.id, row);
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
  // The incremental path has to move every attribute the full render sets, or a
  // row that was patched rather than rebuilt keeps the previous session's
  // tooltip and tone.
  const costCell = wideMeta.querySelector("[data-row-cost]");
  costCell.textContent = cost(session.cost_usd);
  costCell.classList.toggle("row-budget-spent", Boolean(session.budget_exhausted));
  costCell.title = costTitle(session);
  // Conditional markup, so this cell is created and removed rather than written
  // to — the same shape the unread dot uses above. A row that becomes a team
  // lead mid-session gains the cell without being rebuilt, and one whose team
  // ends loses it rather than keeping the last names it had.
  const teamNames = Array.isArray(session.teammates) ? session.teammates : [];
  const existingTeam = wideMeta.querySelector("[data-row-team]");
  if (teamNames.length === 0) {
    existingTeam?.closest("span")?.remove();
  } else {
    const joined = teamNames.join(", ");
    const detail = t("team-explained", { names: joined, count: teamNames.length });
    if (existingTeam) {
      existingTeam.textContent = joined;
      existingTeam.title = detail;
    } else {
      const cell = document.createElement("span");
      const caption = document.createElement("small");
      caption.textContent = "TEAM";
      const value = document.createElement("b");
      value.dataset.rowTeam = "";
      value.textContent = joined;
      value.title = detail;
      cell.append(caption, value);
      wideMeta.append(cell);
    }
  }
  const memoryCell = wideMeta.querySelector("[data-row-memory]");
  memoryCell.textContent = memory(session.memory_bytes);
  memoryCell.classList.toggle("row-memory-limited", Boolean(session.memory_limited));
  memoryCell.title = memoryTitle(session);

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
  costTitle,
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
  memoryTitle,
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

function demoTerminalText(session) {
  return [
    `\x1b[38;5;111m${t("demo-terminal-header")}\x1b[0m`,
    t("demo-terminal-note"),
    "",
    t("demo-terminal-status", { status: session?.status ?? "unknown" }),
    t("demo-terminal-focus"),
    "",
  ].join("\r\n");
}

function renderDemoTerminal(session) {
  if (!state.demoMode || !session) return;
  state.terminal?.reset();
  state.terminal?.write(demoTerminalText(session));
}

function enterFirstRunDemo() {
  if (state.demoMode) return;
  state.demoPrevious = {
    sessions: state.sessions,
    focused: state.focused,
    admission: state.admission,
  };
  state.demoMode = true;
  state.sessions = createFirstRunDemoSessions();
  state.focused = state.sessions[0]?.id ?? null;
  state.admission = {
    ...state.admission,
    max_live_sessions: 11,
    live_sessions: state.sessions.length,
    queued_sessions: state.sessions.filter((session) => session.status === "queued").length,
    aggregate_cost_usd: state.sessions.reduce((total, session) => total + session.cost_usd, 0),
  };
  markFirstRunStep("demo");
  renderRows();
  updateTerminalHeader();
  renderDemoTerminal(state.sessions[0]);
  showToast(t("demo-status-coverage", { count: demoStatusCount(state.sessions) }), "success");
}

function exitFirstRunDemo() {
  if (!state.demoMode) return;
  const previous = state.demoPrevious ?? { sessions: [], focused: null, admission: state.admission };
  state.demoMode = false;
  state.demoPrevious = null;
  state.sessions = previous.sessions;
  state.focused = previous.focused;
  state.admission = previous.admission;
  state.outputChannel = null;
  state.terminal?.reset();
  renderRows();
  updateTerminalHeader();
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
  if (state.demoMode) exitFirstRunDemo();
  state.snapshotLoading = true;
  state.snapshotEvents = [];
  renderSnapshotLoading();
  // Only the snapshot call itself decides whether the daemon is reachable.
  // Everything after it -- reattaching the focused pane, redrawing the header --
  // can fail for its own reasons, and treating those as "the daemon is gone"
  // sent the whole window to the first-run check while the fleet it had just
  // loaded sat behind it. The state that produced it is real: a focused id
  // naming a session the snapshot does not contain.
  let snapshot;
  try {
    snapshot = await invoke("fleet_snapshot");
  } catch (error) {
    state.preflightReason = `Daemon unavailable: ${error}`;
    state.preflightMode = true;
    syncPreflightVisibility();
    syncReviewVisibility();
    state.snapshotLoading = false;
    renderSnapshotLoading();
    void loadPreflight(true);
    return;
  }
  try {
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
    // The fleet is loaded and correct; one pane did not reattach. Say so where
    // the operator is looking rather than replacing the window they were using.
    showToast(t("terminal-attach-failed", { error: String(error) }));
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
  if (state.demoMode && isFirstRunDemoSession(id)) {
    renderDemoTerminal(state.sessions.find((session) => session.id === id));
    return;
  }
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
    // The window breakdown sits with the rollup because it is the same
    // arithmetic asked a different question: not what a session has cost, but
    // what it spent inside the window a provider is currently refusing.
    renderWindowShares(state.admission, sessions, {
      escape: escapeHtml,
      translate: t,
      cost: (usd) => formatCost(usd),
      hours: Number(state.admission?.spend_window_hours) || 24,
    }),
    `<section class="rollup-section rollup-total"><h3>${escapeHtml(t("rollup-total"))}</h3><p><b>${escapeHtml(
      formatCost(totals.priced ? totals.cost_usd : null),
    )}</b> · ${escapeHtml(String(totals.requests))} ${escapeHtml(t("rollup-requests"))}</p></section>`,
  ].join("");
}

function openRollup() {
  const dialog = $("rollup-dialog");
  if (!dialog.open) dialog.showModal();
  renderGuarded($("rollup-body"), t("rollup-render-error"), "openRollup", openRollup, renderRollup);
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
  const dialog = $("broadcast-dialog");
  openWorkspacePage(dialog);
  renderGuarded(
    $("broadcast-list"),
    t("broadcast-render-error"),
    "openBroadcast",
    openBroadcast,
    renderBroadcast,
  );
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
// Workspace utility pages live in `workspacePages.js`.

// Queue behavior lives in `queuePanel.js`; the shell keeps only its row bindings.
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
  const dialog = $("explainer-dialog");
  openWorkspacePage(dialog);
  renderGuarded(
    $("explainer-states"),
    t("explainer-render-error"),
    "openExplainer",
    openExplainer,
    renderExplainerStates,
  );
}

function renderExplainerStates() {
  $("explainer-states").innerHTML = STATUS_KEYS.map((status) => {
    const meta = STATUS_META[status];
    return `<div class="explainer-state"><dt><span class="state-chip tone-${escapeHtml(meta.tone)}"><span class="state-chip-glyph" aria-hidden="true">${meta.glyph}</span><span>${escapeHtml(t(meta.short))}</span></span></dt><dd>${escapeHtml(t(`${meta.short}-explained`))}</dd></div>`;
  }).join("");
}

// Focused terminal history and find controls live in `terminalHistory.js`.
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

// The terminal pane lives in `terminalPane.js`. Bound here rather than
// imported as plain functions because it reads the live `state` object and the
// helpers this module owns.
const workRunPanel = createWorkRunPanel({
  $,
  state,
  invoke,
  showToast,
  t,
  escapeHtml,
  hasOpenWork,
  openWorkspacePage,
});
const {
  renderWorkRun,
  refreshWorkRun,
  setWorkRunPaused,
  workEntryAction,
  renderWorkSchedule,
  refreshWorkSchedule,
  setWorkSchedule,
  setWorkSchedulePaused,
  renderPromptLibrary,
  loadStoredPrompts,
  openPromptLibrary,
  newStoredPrompt,
  selectStoredPrompt,
  saveStoredPrompt,
  deleteStoredPrompt,
  listedProjects,
  startWorkRun,
} = workRunPanel;

const workspacePages = createWorkspacePages({
  $,
  state,
  invoke,
  t,
  escapeHtml,
  renderDataError,
  openWorkspacePage,
  sortProjects,
  hasOpenWork,
  summarizeProjects,
  openItemsCell,
  stalenessLabel,
  renderWorkingSet,
  renderRestoreOutcomes,
  summarizeRestore,
  renderSearchResults,
  searchSummary,
  renderSessionHistory,
  renderWorktrees,
  focusSession,
  openLauncher,
  schedulePreview,
  loadProjectTemplates,
  loadProjectRoots,
  loadStoredPrompts,
  refreshWorkRun,
  refreshWorkSchedule,
  showToast,
  pendingApprovals,
  renderApprovals,
  requestLine,
  waitingSince,
  relativeDwell,
  systemTimeMs,
});
const {
  renderProjects,
  openApprovals,
  openWorkingSets,
  refreshWorkingSets,
  restoreWorkingSet,
  runFleetSearch,
  openSessionHistory,
  refreshWorktrees,
  openProjects,
  renderApprovalInbox,
} = workspacePages;

const terminalHistory = createTerminalHistory({
  $,
  state,
  invoke,
  t,
  showToast,
  Channel,
  document,
  writeTerminalBytes,
});
const {
  toggleFind,
  runFind,
  renderFindCount,
  loadOlderOutput,
  attachSessionOutput,
} = terminalHistory;

const operationalPanels = createOperationalPanels({
  $,
  state,
  invoke,
  t,
  escapeHtml,
  systemTimeMs,
  STATUS_META,
  PREFLIGHT_META,
  STATUS_KEYS,
  countMessage,
  statusLabel,
  metaLabel,
  lifecycleLabel,
  lifecycleTone,
  lifecycleDetail,
  dwell,
  renderDataError,
  closeWorkspacePages,
  syncRailPage,
  syncReviewVisibility,
  renderRows,
  loadSnapshot,
  showToast,
  setReviewMode,
  loadReview,
  markReviewed,
  landSession,
});
const {
  renderDiagnostics,
  renderLogs,
  syncDiagnosticsVisibility,
  setDiagnosticsMode,
  setLogsMode,
  setScreenReaderMode,
  appendLogs,
  syncPreflightVisibility,
  setPreflightMode,
  preflightChecksNeedAttention,
  renderPreflight,
  loadPreflight,
  handlePreflightAction,
  bindEvents: bindOperationalEvents,
} = operationalPanels;

const daemonEvents = createDaemonEvents({
  updateSession,
  removeSession,
  showAttentionToast,
  retractAttentionToast,
});
const { handleDaemonEvent } = daemonEvents;

const terminalPane = createTerminalPane({
  $,
  state,
  invoke,
  showToast,
  t,
  scheduleFit,
  renderFindCount,
  Terminal,
  FitAddon,
  SearchAddon,
  Unicode11Addon,
  WebglAddon,
  DEFAULT_COLS,
  DEFAULT_ROWS,
});
const { openSessionLink, setupTerminal } = terminalPane;


const { bindEvents } = createEventBindings({
  $,
  applyFleetOrder,
  beginFleetOrderFreeze,
  bindLauncherEvents,
  bindOperationalEvents,
  bindQueueEvents,
  checkForUpdates,
  deleteStoredPrompt,
  document,
  enterFirstRunDemo,
  exitFirstRunDemo,
  fitTerminal,
  GROUP_MODES,
  invoke,
  loadExternal,
  loadOlderOutput,
  loadSnapshot,
  newStoredPrompt,
  openApprovals,
  openBroadcast,
  openExplainer,
  openLauncher,
  openProjects,
  openPromptLibrary,
  openRollup,
  openSessionHistory,
  openSessionLink,
  openSettings,
  openWorkspacePage,
  openWorkingSets,
  registerProjectRoot,
  refreshWorkRun,
  refreshWorkingSets,
  RELEASES_PAGE,
  renderProjects,
  renderRows,
  renderStoreQuarantine,
  releaseFleetOrderFreeze,
  runFind,
  runFleetSearch,
  saveSettings,
  saveStoredPrompt,
  selectStoredPrompt,
  sendBroadcast,
  setWorkRunPaused,
  setWorkSchedule,
  setWorkSchedulePaused,
  showToast,
  startWorkRun,
  state,
  syncBroadcastSelection,
  t,
  toggleFind,
  wireOverflowMenus,
  wireRailNavigation,
});
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
