/**
 * Bridges daemon push events to the live session and notification state.
 * Keeping this translation small makes the shell's startup wiring easier to
 * audit without coupling the event protocol to the terminal pane.
 */
export function createDaemonEvents(deps) {
  const { updateSession, removeSession, showAttentionToast, retractAttentionToast } = deps;

  function handleDaemonEvent(event) {
    switch (event.kind) {
      case "session-updated":
        updateSession(event.session);
        break;
      case "session-removed":
        removeSession(event.id);
        break;
      case "notification":
        if (event.event?.kind === "raised") showAttentionToast(event.event.notification);
        if (event.event?.kind === "retracted") retractAttentionToast(event.event.dedup_key);
        break;
      default:
        break;
    }
  }

  return { handleDaemonEvent };
}
