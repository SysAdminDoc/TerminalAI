/**
 * Fleet ordering, grouping, and structured filters.
 *
 * Free-text search remains a shell concern because it spans row fields; these
 * controls are exact dimensions with one shared urgency-aware ordering policy.
 */
export const GROUP_MODES = ["none", "folder", "agent", "status"];

export function createFleetGrouping(deps) {
  const {
    $,
    escapeHtml,
    folderLabel,
    isAttention,
    state,
    statusLabel,
    STATUS_ORDER,
    systemTimeMs,
    t,
  } = deps;

  const STATUS_FILTERS = {
    all: () => true,
    attention: (session) => isAttention(session),
    working: (session) => ["working", "thinking"].includes(session.status),
    idle: (session) => session.status === "idle",
    blocked: (session) => session.status === "rate-limited",
    exited: (session) => session.status === "exited",
  };

  function sortedSessions() {
    return [...state.sessions].sort((a, b) => {
      const status = (STATUS_ORDER[b.status] ?? 0) - (STATUS_ORDER[a.status] ?? 0);
      if (status !== 0) return status;
      return systemTimeMs(a.status_since) - systemTimeMs(b.status_since);
    });
  }

  /// Which group a session belongs to under the current mode.
  function groupOf(session) {
    switch (state.groupBy) {
      case "folder":
        return folderLabel(session.cwd);
      case "agent":
        return session.agent === "codex" ? "Codex" : "Claude Code";
      case "status":
        return statusLabel(session.status);
      default:
        return "";
    }
  }

  /// Apply the structured filters. Returns true when the session survives.
  function passesFilters(session) {
    if (state.agentFilter !== "all" && session.agent !== state.agentFilter) return false;
    const status = STATUS_FILTERS[state.statusFilter] ?? STATUS_FILTERS.all;
    return status(session);
  }

  /// Order sessions so members of a group are adjacent, without disturbing the
  /// attention-first ordering inside each group.
  function applyGrouping(sessions) {
    if (state.groupBy === "none") return sessions;
    const groups = new Map();
    for (const session of sessions) {
      const key = groupOf(session);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(session);
    }
    // Groups are ordered by their most urgent member, so the folder holding a
    // session that needs you appears first.
    const ranked = [...groups.entries()].sort((left, right) => {
      const urgency = (entries) => Math.max(...entries.map((s) => STATUS_ORDER[s.status] ?? 0));
      const delta = urgency(right[1]) - urgency(left[1]);
      return delta !== 0 ? delta : left[0].localeCompare(right[0]);
    });
    return ranked.flatMap(([, entries]) => entries);
  }

  function syncFilterControls() {
    const group = $("group-toggle");
    group.textContent = t(`group-${state.groupBy}`);
    group.classList.toggle("wide-toggle-active", state.groupBy !== "none");
    group.setAttribute("aria-pressed", String(state.groupBy !== "none"));
    for (const [id, value] of [
      ["agent-filter", state.agentFilter],
      ["status-filter", state.statusFilter],
    ]) {
      const select = $(id);
      if (select && select.value !== value) select.value = value;
    }
  }

  /// A chip naming the row's group, shown only while grouping is on.
  function groupChip(session) {
    if (state.groupBy === "none") return "";
    const group = groupOf(session);
    if (!group) return "";
    return `<span class="row-group" title="${escapeHtml(t("row-group"))}">${escapeHtml(group)}</span>`;
  }

  return {
    applyGrouping,
    groupChip,
    groupOf,
    passesFilters,
    sortedSessions,
    syncFilterControls,
  };
}
