/**
 * Live session reconciliation and operator attention notifications.
 *
 * Daemon pushes and snapshot replay must mutate the same state object, announce
 * the same transitions, and clear the same focus/toast state when a session
 * disappears. Keeping that lifecycle together prevents event-order drift.
 */
export function createSessionState(deps) {
  const {
    $,
    document,
    focusSession,
    folderLabel,
    isAttention,
    lifecycleLabel,
    metaLabel,
    renderApprovalInbox,
    renderRows,
    resetTerminal,
    state,
    STATUS_META,
    t,
    updateTerminalHeader,
  } = deps;

  function showAttentionToast(notification) {
    if (state.attentionToasts.has(notification.dedup_key)) return;
    const session = state.sessions.find((item) => item.id === notification.session_id);
    const meta = STATUS_META[notification.status] ?? STATUS_META["needs-you"];
    const toast = document.createElement("button");
    toast.type = "button";
    toast.className = "toast toast-attention toast-visible";
    toast.textContent =
      `${session?.name ?? notification.session_id} · ${metaLabel(meta)} · ${folderLabel(notification.group_key)}`;
    toast.title = t("action-focus-terminal");
    toast.addEventListener("click", () => void focusSession(notification.session_id));
    $("toast-region").append(toast);
    state.attentionToasts.set(notification.dedup_key, { toast, sessionId: notification.session_id });
  }

  function retractAttentionToast(dedupKey) {
    const entry = state.attentionToasts.get(dedupKey);
    if (!entry) return;
    state.attentionToasts.delete(dedupKey);
    entry.toast.classList.remove("toast-visible");
    setTimeout(() => entry.toast.remove(), 240);
  }

  function announceStatusChange(session, previousStatus) {
    if (!previousStatus || previousStatus === session.status) return;
    if (!isAttention(session)) return;
    state.announcementQueue.set(session.id, {
      name: session.name,
      label: lifecycleLabel(session),
    });
    if (state.announcementTimer !== null) return;
    state.announcementTimer = setTimeout(flushAnnouncements, 2000);
  }

  function flushAnnouncements() {
    state.announcementTimer = null;
    if (!state.announcementQueue.size) return;
    const entries = Array.from(state.announcementQueue.values());
    state.announcementQueue.clear();
    const message = entries.length === 1
      ? t("announcement-one", { name: entries[0].name, status: entries[0].label })
      : t("announcement-many", { count: entries.length, names: entries.map((entry) => entry.name).join(", ") });
    const announcer = $("fleet-announcer");
    announcer.textContent = "";
    setTimeout(() => {
      announcer.textContent = message;
    }, 0);
  }

  function applySessionUpdate(session, announce = true) {
    const index = state.sessions.findIndex((item) => item.id === session.id);
    const previous = index === -1 ? null : state.sessions[index];
    if (index === -1) state.sessions.push(session);
    else state.sessions[index] = session;
    if (announce && previous) announceStatusChange(session, previous.status);
    renderRows();
    renderApprovalInbox();
    updateTerminalHeader();
  }

  function updateSession(session) {
    if (state.snapshotLoading) state.snapshotEvents.push({ kind: "session-updated", session });
    applySessionUpdate(session);
  }

  function applySessionRemoval(id) {
    state.sessions = state.sessions.filter((session) => session.id !== id);
    for (const [key, entry] of state.attentionToasts) {
      if (entry.sessionId === id) retractAttentionToast(key);
    }
    if (state.focused === id) {
      state.focused = null;
      resetTerminal("Session exited");
    }
    renderRows();
  }

  function removeSession(id) {
    if (state.snapshotLoading) state.snapshotEvents.push({ kind: "session-removed", id });
    applySessionRemoval(id);
  }

  return {
    announceStatusChange,
    applySessionRemoval,
    applySessionUpdate,
    flushAnnouncements,
    removeSession,
    retractAttentionToast,
    showAttentionToast,
    updateSession,
  };
}
