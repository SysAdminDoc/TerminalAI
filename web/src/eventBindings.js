/**
 * Bind the persistent shell controls to the feature modules.
 *
 * Keeping this coordinator injectable makes the event surface explicit and
 * prevents the Tauri entry module from becoming a second feature module.
 */
export function createEventBindings(deps) {
  const {
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
  } = deps;

  function bindEvents() {
    $("new-session-button").addEventListener("click", () => {
      exitFirstRunDemo();
      openLauncher();
    });
    $("empty-new-button").addEventListener("click", openLauncher);
    $("empty-demo-button").addEventListener("click", enterFirstRunDemo);
    $("demo-exit-button").addEventListener("click", exitFirstRunDemo);
    $("fleet-summary").addEventListener("click", (event) => {
      if (event.target?.closest?.("#fleet-spend")) openRollup();
    });
    $("refresh-button").addEventListener("click", () => {
      exitFirstRunDemo();
      void loadSnapshot();
      void loadExternal();
    });
    $("dismiss-store-quarantine").addEventListener("click", () => {
      state.storeQuarantineDismissed = true;
      renderStoreQuarantine();
    });
    bindOperationalEvents();
    $("update-check-button").addEventListener("click", () => void checkForUpdates());
    $("update-open-releases").addEventListener("click", () => void openSessionLink(RELEASES_PAGE));
    $("filter-input").addEventListener("input", renderRows);
    $("agent-filter").addEventListener("change", (event) => {
      state.agentFilter = event.target.value;
      renderRows();
    });
    $("status-filter").addEventListener("change", (event) => {
      state.statusFilter = event.target.value;
      renderRows();
    });
    $("group-toggle").addEventListener("click", () => {
      // Cycles rather than opening a menu: four modes is fewer clicks this way,
      // and the button always states the current one.
      const next = (GROUP_MODES.indexOf(state.groupBy) + 1) % GROUP_MODES.length;
      state.groupBy = GROUP_MODES[next];
      renderRows();
    });
    $("fleet-list").addEventListener("mouseenter", beginFleetOrderFreeze);
    $("fleet-list").addEventListener("mouseleave", releaseFleetOrderFreeze);
    $("fleet-list").addEventListener("focusin", beginFleetOrderFreeze);
    $("fleet-list").addEventListener("focusout", releaseFleetOrderFreeze);
    $("apply-fleet-order").addEventListener("click", applyFleetOrder);
    $("wide-toggle").addEventListener("click", () => {
      state.wideMode = !state.wideMode;
      renderRows();
    });
    $("attention-filter").addEventListener("click", () => {
      state.attentionOnly = !state.attentionOnly;
      $("attention-filter").setAttribute("aria-pressed", String(state.attentionOnly));
      $("attention-filter").classList.toggle("attention-filter-active", state.attentionOnly);
      renderRows();
    });
    document.addEventListener("keydown", (event) => {
      const tag = event.target?.tagName?.toLowerCase();
      if (event.key === "/" && !event.target?.isContentEditable && !["input", "textarea", "select"].includes(tag)) {
        event.preventDefault();
        $("filter-input").focus({ preventScroll: true });
        $("filter-input").select();
      }
    });
    bindLauncherEvents();
    $("cancel-launch-button").addEventListener("click", () => $("launcher-dialog").close());
    $("close-launcher-button").addEventListener("click", () => $("launcher-dialog").close());
    $("close-rollup-button").addEventListener("click", () => $("rollup-dialog").close());
    $("close-queue-button").addEventListener("click", () => $("queue-dialog").close());
    bindQueueEvents();
    wireOverflowMenus($("app-menu-button").ownerDocument);
    $("explainer-toggle").addEventListener("click", () => openExplainer());
    $("settings-toggle").addEventListener("click", () => void openSettings());
    $("history-toggle").addEventListener("click", () => void openSessionHistory());
    $("close-history-button").addEventListener("click", () => $("history-dialog").close());
    $("fleet-search-toggle").addEventListener("click", () => {
      const dialog = $("search-dialog");
      openWorkspacePage(dialog);
      $("fleet-search-input").focus();
      $("fleet-search-input").select();
    });
    $("close-search-button").addEventListener("click", () => $("search-dialog").close());
    $("approvals-toggle").addEventListener("click", () => openApprovals());
    $("close-approvals-button").addEventListener("click", () => $("approvals-dialog").close());
    $("working-sets-toggle").addEventListener("click", () => void openWorkingSets());
    $("close-working-sets-button").addEventListener("click", () => $("working-sets-dialog").close());
    $("working-set-save").addEventListener("click", async () => {
      const name = $("working-set-name").value.trim();
      if (!name) {
        showToast(t("working-sets-needs-name"));
        return;
      }
      try {
        const count = await invoke("save_working_set", { name, groupBy: state.groupBy });
        $("working-set-name").value = "";
        showToast(t("working-sets-saved", { name, count }), "success");
        await refreshWorkingSets();
      } catch (error) {
        showToast(String(error));
      }
    });
    $("fleet-search-run").addEventListener("click", () => void runFleetSearch());
    $("fleet-search-input").addEventListener("keydown", (event) => {
      if (event.key === "Enter") void runFleetSearch();
    });
    $("save-settings-button").addEventListener("click", () => void saveSettings());
    $("close-settings-button").addEventListener("click", () => $("settings-dialog").close());
    $("empty-explainer-button").addEventListener("click", () => openExplainer());
    $("close-explainer-button").addEventListener("click", () => $("explainer-dialog").close());
    $("empty-root-button").addEventListener("click", () => void registerProjectRoot());
    $("projects-toggle").addEventListener("click", () => void openProjects());
    $("close-projects-button").addEventListener("click", () => $("projects-dialog").close());
    $("prompts-toggle").addEventListener("click", () => openPromptLibrary());
    $("close-prompt-button").addEventListener("click", () => $("prompt-dialog").close());
    $("prompt-new-button").addEventListener("click", newStoredPrompt);
    $("prompt-save-button").addEventListener("click", () => void saveStoredPrompt());
    $("stored-prompt-list").addEventListener("click", (event) => {
      const select = event.target.closest("button[data-prompt-select]");
      if (select) {
        selectStoredPrompt(select.dataset.promptSelect);
        return;
      }
      const remove = event.target.closest("button[data-prompt-delete]");
      if (remove) void deleteStoredPrompt(remove.dataset.promptDelete);
    });
    $("projects-open-only").addEventListener("change", () => renderProjects());
    $("work-start-button").addEventListener("click", () => void startWorkRun());
    $("work-pause-button").addEventListener("click", () => void setWorkRunPaused(true));
    $("work-resume-button").addEventListener("click", () => void setWorkRunPaused(false));
    $("work-repeat-select").addEventListener("change", () => void setWorkSchedule());
    $("work-schedule-pause-button").addEventListener("click", () => void setWorkSchedulePaused(true));
    $("work-schedule-resume-button").addEventListener("click", () => void setWorkSchedulePaused(false));
    $("work-clear-button").addEventListener("click", async () => {
      try {
        await invoke("clear_work_run");
      } catch (error) {
        showToast(String(error));
      }
      await refreshWorkRun();
    });
    $("broadcast-toggle").addEventListener("click", () => openBroadcast());
    $("broadcast-list").addEventListener("change", () => syncBroadcastSelection());
    $("cancel-broadcast-button").addEventListener("click", () => $("broadcast-dialog").close());
    $("send-broadcast-button").addEventListener("click", () => void sendBroadcast());
    $("terminal-clear").addEventListener("click", () => state.terminal?.clear());
    $("terminal-history").addEventListener("click", () => void loadOlderOutput());
    $("terminal-find-toggle").addEventListener("click", () => toggleFind());
    $("terminal-find-close").addEventListener("click", () => toggleFind(false));
    $("terminal-find-input").addEventListener("input", () => runFind());
    $("terminal-find-next").addEventListener("click", () => runFind("next"));
    $("terminal-find-previous").addEventListener("click", () => runFind("previous"));
    $("terminal-resize").addEventListener("click", async () => {
      if (!state.focused) return;
      try {
        state.lastSentSize = null;
        fitTerminal();
        showToast(t("terminal-refitted", { cols: state.terminal.cols, rows: state.terminal.rows }), "success");
      } catch (error) {
        showToast(String(error));
      }
    });
    wireRailNavigation();
  }
  

  return { bindEvents };
}
