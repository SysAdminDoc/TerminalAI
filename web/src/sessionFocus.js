/**
 * Focused-session switching.
 *
 * The coordinator serializes attach/restore work and owns stale-failure
 * rollback. Terminal history owns the channel itself; the fleet shell owns the
 * render callbacks supplied here.
 */
export function createSessionFocus(deps) {
  const {
    attachSessionOutput,
    fitTerminal,
    isFirstRunDemoSession,
    renderDemoTerminal,
    renderRows,
    showToast,
    state,
    t,
    updateTerminalHeader,
  } = deps;

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
        showToast(
          t("focus-session-error", {
            error: String(error) + "; " + String(restoreError),
          }),
        );
        return;
      }
      if (state.focused !== previousFocused) return;
      updateTerminalHeader();
      renderRows();
      showToast(t("focus-session-error", { error: String(error) }));
    }
  }

  return { focusSession, focusSessionNow };
}
