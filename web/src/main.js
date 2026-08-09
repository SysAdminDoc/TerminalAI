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
import { RELEASES_PAGE } from "./updateCheck.js";
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
import { createSessionState } from "./sessionState.js";
import { createTerminalLayout, DEFAULT_COLS, DEFAULT_ROWS } from "./terminalLayout.js";
import { createFleetRowState } from "./fleetRowState.js";
import { createFleetGrouping, GROUP_MODES } from "./fleetGrouping.js";
import { createFleetList } from "./fleetList.js";
import {
  createFirstRunDemoSessions,
  demoStatusCount,
  isFirstRunDemoSession,
  readFirstRunProgress,
} from "./firstRun.js";
import { createFirstRunGuide } from "./firstRunGuide.js";
import {
  createRendererUtils,
  createTerminalOutput,
  escapeHtml,
  invokeArgs,
} from "./rendererUtils.js";
import { createSessionPresentation } from "./sessionPresentation.js";
import { createFleetNotices } from "./fleetNotices.js";
import { createShellNavigation } from "./shellNavigation.js";
import { createReviewVisibility } from "./reviewVisibility.js";
import { createUpdatePanel } from "./updatePanel.js";

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

const {
  answerCountdownLabel,
  cost,
  costTitle,
  dwell,
  folderLabel,
  isAttention,
  memory,
  memoryTitle,
  ports,
  toolProgress,
} = createSessionPresentation({ relativeDwell, t });

const { renderDataError, renderGuarded, showToast } = createRendererUtils({ $, document, t });
const { writeTerminalBytes } = createTerminalOutput({ state });

const shellNavigation = createShellNavigation({
  $,
  document,
  setPreflightMode: (...args) => setPreflightMode(...args),
  state,
});
const { closeWorkspacePages, openWorkspacePage, syncRailPage, wireRailNavigation } = shellNavigation;
const fleetNotices = createFleetNotices({ $, state, t });
const { renderAuthBanner, renderStoreQuarantine, renderStoreWriteError } = fleetNotices;
const firstRunGuide = createFirstRunGuide({ $, state, t });
const { markFirstRunStep, renderFirstRunGuide } = firstRunGuide;
const { syncReviewVisibility } = createReviewVisibility({ $, state, t });

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

const updatePanel = createUpdatePanel({
  $,
  fallbackVersion: FALLBACK_APP_VERSION,
  invoke,
  showToast,
  state,
  t,
});
const { checkForUpdates } = updatePanel;

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


const fleetGrouping = createFleetGrouping({
  $,
  escapeHtml,
  folderLabel,
  isAttention,
  state,
  statusLabel,
  STATUS_ORDER,
  systemTimeMs,
  t,
});
const { applyGrouping, groupChip, groupOf, passesFilters, sortedSessions, syncFilterControls } = fleetGrouping;




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




const fleetRowState = createFleetRowState({
  $,
  answerCountdownLabel,
  cost,
  costTitle,
  document,
  dwell,
  escapeHtml,
  focusSession: (...args) => focusSession(...args),
  folderLabel,
  groupOf,
  isAttention,
  lastActivity,
  lifecycleDetail,
  lifecycleLabel,
  lifecycleTone,
  memory,
  memoryTitle,
  ports,
  queueGlyph,
  queueTitle,
  reconcileGroupChip,
  renderRow,
  renderRows: (...args) => renderRows(...args),
  rowAction: (...args) => rowAction(...args),
  showToast,
  state,
  STATUS_META,
  t,
  toolProgress,
});
const { createFleetRow, updateFleetRow } = fleetRowState;

const fleetList = createFleetList({
  $,
  applyGrouping,
  countMessage,
  createFleetRow,
  document,
  folderLabel,
  isAttention,
  lastActivity,
  lifecycleLabel,
  passesFilters,
  ports,
  reconcileKeyedRows,
  renderAuthBanner,
  renderFirstRunGuide,
  renderPinnedSplit,
  renderSummary,
  sortedSessions,
  state,
  syncFilterControls,
  syncReviewVisibility,
  t,
  toolProgress,
  updateFleetRow,
});
const {
  applyFleetOrder,
  beginFleetOrderFreeze,
  pendingPriorityChanges,
  releaseFleetOrderFreeze,
  renderRows,
  renderSnapshotLoading,
} = fleetList;

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
const terminalLayout = createTerminalLayout({
  $,
  invoke,
  state,
});
const { fitTerminal, scheduleFit } = terminalLayout;


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
  rowAction,
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

const sessionState = createSessionState({
  $,
  document,
  focusSession,
  folderLabel,
  isAttention,
  lifecycleLabel,
  metaLabel,
  renderApprovalInbox,
  renderRows,
  resetTerminal,
  state,
  STATUS_META,
  t,
  updateTerminalHeader,
});
const {
  applySessionRemoval,
  applySessionUpdate,
  removeSession,
  retractAttentionToast,
  showAttentionToast,
  updateSession,
} = sessionState;

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
