/** Small shell state transitions shared by session and operational panels. */
export function createShellModes({
  $,
  loadReview,
  renderRows,
  setPreflightMode,
  state,
  syncReviewVisibility,
  t,
  updateTerminalHeader,
}) {
  function resetTerminal(status = t("empty-waiting-for-session")) {
    state.terminal?.reset();
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

  return { resetTerminal, setReviewMode };
}
