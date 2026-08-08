/**
 * Fleet snapshot refresh coordination.
 *
 * A snapshot is a transport read followed by a bounded reconciliation: events
 * that arrive during the read are replayed, then the focused terminal is
 * reattached. Preflight owns daemon policy; this module owns the ordering.
 */
export function createSnapshotCoordinator(deps) {
  const {
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
  } = deps;

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
    // Everything after it -- reattaching the focused pane, redrawing the header
    // -- can fail for its own reasons, and treating those as daemon loss sent
    // the whole window to the first-run check while the loaded fleet sat behind
    // it. The loaded state is real, even if its focused pane is not.
    let snapshot;
    try {
      snapshot = await invoke("fleet_snapshot");
    } catch (error) {
      state.preflightReason = "Daemon unavailable: " + String(error);
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

  return { loadSnapshot, loadSnapshotNow };
}
