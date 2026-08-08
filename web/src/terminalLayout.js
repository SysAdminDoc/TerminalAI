/**
 * Focused-terminal geometry and resize delivery.
 *
 * xterm construction lives in terminalPane.js; this boundary owns measuring
 * the host, debouncing splitter changes, and sending each session its own
 * geometry exactly once per settled size.
 */
export const DEFAULT_COLS = 120;
export const DEFAULT_ROWS = 40;

export function createTerminalLayout(deps) {
  const { $, invoke, state } = deps;

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

  return { fitTerminal, scheduleFit, terminalSizeLabel };
}
