/**
 * Offline first-run demo mode.
 *
 * Demo rows are a local teaching surface, not a second daemon. This boundary
 * owns the reversible state swap and terminal copy while the fleet shell keeps
 * the normal row and focus behavior.
 */
export function createSessionDemo(deps) {
  const {
    createFirstRunDemoSessions,
    demoStatusCount,
    markFirstRunStep,
    renderRows,
    showToast,
    state,
    t,
    updateTerminalHeader,
  } = deps;

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

  return { demoTerminalText, enterFirstRunDemo, exitFirstRunDemo, renderDemoTerminal };
}
