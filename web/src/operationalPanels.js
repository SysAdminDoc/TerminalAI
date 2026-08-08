/**
 * Diagnostics, daemon logs, and preflight checks.
 *
 * These operational views all switch the shell away from the fleet and share
 * the same live state, so they move together while keeping their policy and
 * error rendering in one auditable module.
 */
export function createOperationalPanels(deps) {
  const {
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
  } = deps;

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
  return Number.isFinite(time)
    ? new Date(time).toISOString().replace(".000Z", "Z").replace("T", " ")
    : t("unknown-time");
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
      return (
        '<li class="diagnostic-event"><span class="diagnostic-event-glyph tone-' +
        entryMeta.tone +
        '" aria-hidden="true">' +
        entryMeta.glyph +
        '</span><div class="diagnostic-event-body"><div><b>' +
        escapeHtml(metaLabel(entryMeta)) +
        '</b><span>' +
        escapeHtml(t("diagnostics-from", { status: from })) +
        '</span></div><small>' +
        escapeHtml(diagnosticSource(entry.source)) +
        ' · ' +
        escapeHtml(diagnosticTime(entry.at)) +
        '</small>' +
        (reason ? '<p>' + escapeHtml(reason) + '</p>' : '') +
        '</div></li>'
      );
    }).join("")
    : '<li class="diagnostics-empty">' + escapeHtml(t("empty-no-transition-history")) + "</li>";
  const currentDetail = lifecycleDetail(session);
  const currentSummary =
    (currentDetail ? currentDetail + ' · ' : '') +
    t("diagnostics-for", { dwell: dwell(session.status_since) }) +
    ' · ' +
    t("diagnostics-source", { source });
  host.innerHTML =
    '<div class="diagnostics-heading"><div><span class="eyebrow">' +
    escapeHtml(t("diagnostics-why-this-state")) +
    '</span><h2>' +
    escapeHtml(session.name) +
    '</h2><p>' +
    escapeHtml(session.cwd) +
    '</p></div><div class="diagnostics-heading-actions">' +
    '<button type="button" class="button button-quiet" data-diagnostics-action="preflight">' +
    escapeHtml(t("button-preflight")) +
    '</button><span class="status-glyph tone-' +
    lifecycleTone(session, meta) +
    '" title="' +
    escapeHtml(label) +
    '" aria-hidden="true">' +
    meta.glyph +
    '</span></div></div>' +
    '<div class="diagnostics-current"><span>' +
    escapeHtml(t("diagnostics-current-status")) +
    '</span><b>' +
    escapeHtml(label) +
    '</b><span>' +
    escapeHtml(currentSummary) +
    '</span></div>' +
    '<ol class="diagnostics-timeline">' +
    timeline +
    "</ol>";
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
  return Number.isFinite(time)
    ? new Date(time).toISOString().replace(".000Z", "Z").replace("T", " ")
    : t("unknown-time");
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
    return (
      `<li class="log-event"><div class="log-event-heading"><b>${escapeHtml(entry.level ?? "INFO")}</b>` +
      `<span>${escapeHtml(entry.target ?? "terminalai")}</span><time>${escapeHtml(logTime(entry.at))}</time></div>` +
      `<p>${escapeHtml(entry.message ?? "")}</p>${fields ? `<div class="log-event-fields">${fields}</div>` : ""}</li>`
    );
  }).join("");
  host.innerHTML =
    '<div class="logs-heading"><div><span class="eyebrow">' +
    escapeHtml(t("logs-control-plane")) +
    '</span><h2>' +
    escapeHtml(t("logs-daemon-records")) +
    '</h2><p>' +
    escapeHtml(t("logs-latest-retained")) +
    '</p></div><span class="status-glyph tone-sapphire" aria-hidden="true">≋</span></div><ol class="logs-list">' +
    rows +
    '</ol>';
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
  if (active) closeWorkspacePages();
  [
    "fleet-state-strip",
    "column-labels",
    "fleet-list",
    "fleet-order-notice",
    "empty-state",
    "review-view",
  ].forEach((id) => {
    $(id).classList.toggle("view-hidden", active);
  });
  $("preflight-view").classList.toggle("view-hidden", !active);
  $("preflight-toggle").setAttribute("aria-pressed", String(active));
  $("preflight-toggle").classList.toggle("wide-toggle-active", active);
  syncRailPage(active ? "preflight" : "fleet");
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
    return (
      `<article class="preflight-row" role="listitem"><span class="status-glyph tone-${escapeHtml(meta.tone)}" ` +
      `title="${escapeHtml(metaLabel(meta))}" aria-hidden="true">${escapeHtml(meta.glyph)}</span>` +
      `<div class="preflight-copy"><div><b>${escapeHtml(check.label)}</b>` +
      `<span>${escapeHtml(metaLabel(meta))}</span></div><strong>${escapeHtml(check.detected)}</strong>${detail}</div>` +
      `<div class="preflight-actions"><button type="button" class="button button-secondary" ` +
      `data-preflight-action="fix" data-preflight-id="${escapeHtml(check.id)}"${check.can_fix ? "" : " disabled"} ` +
      `aria-label="${escapeHtml(fixLabel)} ${escapeHtml(check.label)}">${escapeHtml(fixLabel)}</button>` +
      `<button type="button" class="button button-quiet" data-preflight-action="recheck" ` +
      `data-preflight-id="${escapeHtml(check.id)}" aria-label="${escapeHtml(t("button-recheck"))} ` +
      `${escapeHtml(check.label)}">${escapeHtml(t("button-recheck"))}</button></div></article>`
    );
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

function bindOperationalEvents() {
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
}


  return {
    diagnosticSource,
    diagnosticTime,
    renderDiagnostics,
    formatReason,
    logTime,
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
  };
}
