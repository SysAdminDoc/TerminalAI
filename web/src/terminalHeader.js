/**
 * Focused-terminal heading and empty-state layout.
 *
 * The xterm host and its placeholder share one box. This boundary owns the
 * layout transition and the live identity/status labels without owning focus
 * switching or terminal construction.
 */
export function createTerminalHeader(deps) {
  const {
    $,
    dwell,
    lifecycleLabel,
    renderDiagnostics,
    scheduleFit,
    state,
    STATUS_META,
    t,
  } = deps;

  // The xterm element is appended after the placeholder inside an overflow-
  // hidden host, so a placeholder left in flow lays the renderer out below the
  // visible box.
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

  return { renderTerminalPlaceholder, updateTerminalHeader };
}
