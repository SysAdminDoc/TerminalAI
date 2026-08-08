/**
 * Admission settings page orchestration.
 *
 * The daemon owns admission policy; this boundary owns reading the dialog,
 * showing live usage beside each limit, and refreshing the fleet after a save.
 */
export function createSettingsPage(deps) {
  const {
    $,
    formatCost,
    invoke,
    loadSnapshot,
    openWorkspacePage,
    showToast,
    state,
    t,
  } = deps;

  /// Read a settings field as an optional number.
  ///
  /// Empty means "no limit", which is a different fact from zero: zero would
  /// ask the daemon for a ceiling of nothing, and the daemon treats that as
  /// disabled anyway, so the two would silently agree by accident rather than
  /// by intent.
  function optionalNumber(id) {
    const raw = $(id).value.trim();
    if (!raw) return null;
    const value = Number(raw);
    return Number.isFinite(value) && value >= 0 ? value : null;
  }

  function updateLimitUsageCard(name, used, limit, format = (value) => String(Math.round(value))) {
    const value = Number(used) || 0;
    const ceiling = Number(limit);
    const bounded = Number.isFinite(ceiling) && ceiling > 0;
    $(`settings-${name}-usage`).textContent = `${format(value)} / ${bounded ? format(ceiling) : "∞"}`;
    const progress = $(`settings-${name}-progress`);
    progress.max = bounded ? ceiling : Math.max(value, 1);
    progress.value = Math.min(value, progress.max);
  }

  async function openSettings() {
    try {
      const settings = await invoke("admission_config");
      $("settings-max-live").value = settings.max_live_sessions ?? "";
      $("settings-default-budget").value = settings.default_budget_usd ?? "";
      $("settings-spend-ceiling").value = settings.spend_ceiling_usd ?? "";
      $("settings-spend-window").value = settings.spend_window_hours
        ? Math.round(settings.spend_window_hours)
        : "";
      $("settings-memory-budget").value = settings.memory_budget_mb ?? "";
      $("settings-memory-cap").value = settings.session_memory_cap_mb ?? "";
      $("settings-max-processes").value = settings.max_processes_per_session ?? "";
      const fromEnvironment = Array.isArray(settings.from_environment) ? settings.from_environment : [];
      const note = $("settings-environment-note");
      note.hidden = fromEnvironment.length === 0;
      note.textContent = fromEnvironment.length
        ? t("settings-from-environment", { names: fromEnvironment.join(", ") })
        : "";
      const live = Number(state.admission?.live_sessions) || 0;
      const spend = Number(state.admission?.spend_window_usd ?? state.admission?.aggregate_cost_usd) || 0;
      const memoryMb = state.sessions.reduce(
        (total, session) => total + (Number(session.memory_bytes) || 0),
        0,
      ) / 1048576;
      updateLimitUsageCard("live", live, settings.max_live_sessions);
      updateLimitUsageCard("spend", spend, settings.spend_ceiling_usd, (value) => formatCost(value));
      updateLimitUsageCard("memory", memoryMb, settings.memory_budget_mb, (value) => `${Math.round(value)} MB`);
      updateLimitUsageCard("process", 0, settings.max_processes_per_session);
      $("settings-process-usage").textContent = settings.max_processes_per_session == null
        ? "∞"
        : `≤ ${settings.max_processes_per_session}`;
      $("settings-error").hidden = true;
      openWorkspacePage($("settings-dialog"));
    } catch (error) {
      showToast(String(error));
    }
  }

  async function saveSettings() {
    const maxLive = Number($("settings-max-live").value.trim());
    if (!Number.isInteger(maxLive) || maxLive < 1) {
      const problem = $("settings-error");
      problem.textContent = t("settings-max-live") + ": 1+";
      problem.hidden = false;
      return;
    }
    const settings = {
      max_live_sessions: maxLive,
      default_budget_usd: optionalNumber("settings-default-budget"),
      spend_ceiling_usd: optionalNumber("settings-spend-ceiling"),
      spend_window_hours: optionalNumber("settings-spend-window") || 24,
      memory_budget_mb: optionalNumber("settings-memory-budget"),
      session_memory_cap_mb: optionalNumber("settings-memory-cap"),
      max_processes_per_session: optionalNumber("settings-max-processes"),
    };
    try {
      await invoke("set_admission", { settings });
      $("settings-dialog").close();
      showToast(t("settings-saved"), "success");
      await loadSnapshot();
    } catch (error) {
      const problem = $("settings-error");
      problem.textContent = String(error);
      problem.hidden = false;
    }
  }

  return { openSettings, optionalNumber, saveSettings, updateLimitUsageCard };
}
