/**
 * Fleet list reconciliation and interaction-safe ordering.
 *
 * Grouping supplies the desired session order and row state supplies the DOM
 * patch. This boundary owns the list surface, empty/loading states, and the
 * short order freeze that keeps active keyboard/mouse work from jumping away.
 */
export function createFleetList(deps) {
  const {
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
  } = deps;

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
      return [
        session.name,
        session.cwd,
        folderLabel(session.cwd),
        session.branch,
        session.agent,
        session.model,
        session.status,
        session.phase,
        lifecycleLabel(session),
        lastActivity(session),
        toolProgress(session.tool_progress),
        session.restarts,
        ports(session.ports),
      ]
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
    $("empty-state").classList.toggle(
      "empty-state-hidden",
      state.snapshotLoading || state.sessions.length > 0,
    );
    list.classList.toggle("fleet-list-hidden", state.sessions.length === 0);
    list.classList.toggle("fleet-list-wide", state.wideMode);
    $("demo-mode-banner").hidden = !state.demoMode;
    renderFirstRunGuide();
    const identityLabel = $("column-identity-label");
    const identityLabelKey = state.wideMode ? "column-label-wide" : "column-label-compact";
    identityLabel.setAttribute("data-i18n", identityLabelKey);
    identityLabel.textContent = t(identityLabelKey);
    const wideToggle = $("wide-toggle");
    const wideTitle = state.wideMode
      ? "button-hide-model-effort-cost"
      : "button-show-model-effort-cost";
    wideToggle.setAttribute("aria-pressed", String(state.wideMode));
    wideToggle.textContent = state.wideMode ? t("button-compact") : t("button-wide");
    wideToggle.setAttribute("data-i18n-title", wideTitle);
    wideToggle.title = t(wideTitle);
    wideToggle.classList.toggle("wide-toggle-active", state.wideMode);
    const rovingId = sessions.find((session) => session.id === state.focused)?.id
      ?? sessions[0]?.id
      ?? null;
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

  return {
    applyFleetOrder,
    beginFleetOrderFreeze,
    fleetListIsInteracting,
    pendingPriorityChanges,
    releaseFleetOrderFreeze,
    renderRows,
    renderOrderNotice,
    renderSnapshotLoading,
  };
}
