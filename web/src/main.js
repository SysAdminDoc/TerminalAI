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
import { rateLimitedLabel } from "./rateLimit.js";
import {
  createSessionStatus,
  STATUS_KEYS,
  STATUS_META,
  STATUS_ORDER,
} from "./sessionStatus.js";
import {
  formatCost,
} from "./rollup.js";
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
import { createRollupPage } from "./rollupPage.js";
import { createExplainerPage } from "./explainerPage.js";
import { createSessionFocus } from "./sessionFocus.js";
import { createSnapshotCoordinator } from "./snapshotCoordinator.js";
import { createLauncher } from "./launcher.js";
import { createQueuePanel } from "./queuePanel.js";
import { createWorkspacePages } from "./workspacePages.js";
import { createTerminalHistory } from "./terminalHistory.js";
import { createOperationalPanels } from "./operationalPanels.js";
import { createDaemonEvents } from "./daemonEvents.js";
import { createEventBindings } from "./eventBindings.js";
import { createReviewPage } from "./reviewPage.js";
import { createExternalSessions } from "./externalSessions.js";
import { createBroadcastPanel } from "./broadcastPanel.js";
import { createFleetSummary } from "./fleetSummary.js";
import { createPinnedPanes } from "./pinnedPanes.js";
import { createSettingsPage } from "./settingsPage.js";
import { createSessionDemo } from "./sessionDemo.js";
import { createTerminalHeader } from "./terminalHeader.js";
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

let loadSnapshot;

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

const reviewPage = createReviewPage({
  $,
  countMessage,
  escapeHtml,
  folderLabel,
  invoke,
  renderDataError,
  showToast,
  state,
  t,
});
const { renderReview, loadReview, landSession, markReviewed } = reviewPage;

const externalSessions = createExternalSessions({
  $,
  countMessage,
  escapeHtml,
  folderLabel,
  invoke,
  metaLabel,
  state,
  t,
});
const { renderExternal, loadExternal } = externalSessions;

const broadcastPanel = createBroadcastPanel({
  $,
  escapeHtml,
  invoke,
  openWorkspacePage,
  renderGuarded,
  showToast,
  state,
  t,
});
const {
  openBroadcast,
  readBroadcastSelection,
  renderBroadcast,
  sendBroadcast,
  syncBroadcastSelection,
} = broadcastPanel;

const rollupPage = createRollupPage({
  $,
  escapeHtml,
  renderGuarded,
  state,
  t,
});
const { openRollup, renderRollup } = rollupPage;

const explainerPage = createExplainerPage({
  $,
  escapeHtml,
  openWorkspacePage,
  renderGuarded,
  t,
});
const { openExplainer, renderExplainerStates } = explainerPage;

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

const fleetSummary = createFleetSummary({
  $,
  escapeHtml,
  metaLabel,
  state,
  t,
});
const { renderSummary } = fleetSummary;

const pinnedPanes = createPinnedPanes({
  $,
  document,
  invoke,
  lifecycleLabel,
  state,
  STATUS_META,
  t,
});
const { renderPinnedSplit, startPinnedPolling } = pinnedPanes;


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



function resetTerminal(status = t("empty-waiting-for-session")) {
  if (state.terminal) state.terminal.reset();
  $("terminal-status").textContent = status;
  updateTerminalHeader();
}

function setReviewMode(active) {
  if (active && state.preflightMode) setPreflightMode(false);
  state.reviewMode = active;
  syncReviewVisibility();
  if (active) void loadReview();
  else renderRows();
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

const terminalHeader = createTerminalHeader({
  $,
  dwell,
  lifecycleLabel,
  renderDiagnostics: (...args) => renderDiagnostics(...args),
  scheduleFit,
  state,
  STATUS_META,
  t,
});
const { renderTerminalPlaceholder, updateTerminalHeader } = terminalHeader;

const sessionDemo = createSessionDemo({
  createFirstRunDemoSessions,
  demoStatusCount,
  markFirstRunStep,
  renderRows,
  showToast,
  state,
  t,
  updateTerminalHeader,
});
const { enterFirstRunDemo, exitFirstRunDemo, renderDemoTerminal } = sessionDemo;

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

const sessionFocus = createSessionFocus({
  attachSessionOutput,
  fitTerminal,
  isFirstRunDemoSession,
  renderDemoTerminal,
  renderRows,
  showToast,
  state,
  t,
  updateTerminalHeader,
});
const { focusSession } = sessionFocus;

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
  loadSnapshot: (...args) => loadSnapshot(...args),
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

const snapshotCoordinator = createSnapshotCoordinator({
  applySessionRemoval,
  applySessionUpdate,
  attachSessionOutput,
  exitFirstRunDemo,
  invoke,
  loadPreflight,
  renderRows,
  renderSnapshotLoading,
  renderStoreQuarantine,
  renderStoreWriteError,
  showToast,
  state,
  syncPreflightVisibility,
  syncReviewVisibility,
  t,
  updateTerminalHeader,
});
loadSnapshot = snapshotCoordinator.loadSnapshot;

const settingsPage = createSettingsPage({
  $,
  formatCost,
  invoke,
  loadSnapshot: (...args) => loadSnapshot(...args),
  openWorkspacePage,
  showToast,
  state,
  t,
});
const { openSettings, saveSettings } = settingsPage;

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
