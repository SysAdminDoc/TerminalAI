/** Coordinate renderer boot without making the entry module own lifecycle code. */
export function createStartup({
  appendLogs,
  bindEvents,
  focusSession,
  handleDaemonEvent,
  listen,
  loadExternal,
  loadPreflight,
  loadPresets,
  loadSnapshot,
  localizeDom,
  renderRows,
  setupTerminal,
  showToast,
  startPinnedPolling,
  syncAgentFields,
  t,
  updateTerminalHeader,
  window,
  wdioBuild = false,
  setInterval: setIntervalImpl,
}) {
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
    setIntervalImpl(() => {
      renderRows();
      updateTerminalHeader();
    }, 1000);
  }

  async function startWhenReady() {
    if (wdioBuild) {
      await new Promise((resolve) => {
        window.addEventListener("terminalai-wdio-ready", resolve, { once: true });
      });
    }
    await start();
  }

  return { start, startWhenReady };
}
