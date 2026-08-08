import { defaultSelection, isEligible, summarize, targets } from "./broadcast.js";

/**
 * Broadcast dialog orchestration.
 *
 * Eligibility and refusal arithmetic stay in `broadcast.js`; this module owns
 * the DOM list, the operator's live selection, and the send boundary.
 */
export function createBroadcastPanel(deps) {
  const {
    $,
    escapeHtml,
    invoke,
    openWorkspacePage,
    renderGuarded,
    showToast,
    state,
    t,
  } = deps;

  /**
   * Send one prompt to several sessions.
   *
   * Every session is listed, ineligible ones included and greyed with the
   * reason, because hiding them makes the fleet look smaller than it is at the
   * moment the operator is deciding who to send to. Only eligible ones start
   * ticked.
   */
  function renderBroadcast() {
    const rows = targets(state.sessions);
    const eligible = rows.filter((row) => !row.reason).length;
    $("broadcast-coverage").textContent = eligible
      ? t("broadcast-eligible", { count: eligible })
      : t("broadcast-none-eligible");
    $("broadcast-list").innerHTML = rows
      .map(({ session, reason }) => {
        const checked = !reason && state.broadcastSelection.includes(session.id) ? " checked" : "";
        const disabled = reason ? " disabled" : "";
        const why = reason
          ? `<small class="broadcast-why">${escapeHtml(t(reason))}</small>`
          : "";
        return [
          `<label class="broadcast-row${reason ? " is-ineligible" : ""}">`,
          `<input type="checkbox" data-broadcast-id="${escapeHtml(session.id)}"`,
          `${checked}${disabled} />`,
          `<span>${escapeHtml(session.id)} · ${escapeHtml(session.name)}</span>`,
          why,
          "</label>",
        ].join("");
      })
      .join("");
    $("send-broadcast-button").disabled = eligible === 0;
  }

  function openBroadcast() {
    state.broadcastSelection = defaultSelection(state.sessions);
    const dialog = $("broadcast-dialog");
    openWorkspacePage(dialog);
    renderGuarded(
      $("broadcast-list"),
      t("broadcast-render-error"),
      "openBroadcast",
      openBroadcast,
      renderBroadcast,
    );
  }

  function readBroadcastSelection() {
    return [...$("broadcast-list").querySelectorAll("input[data-broadcast-id]")]
      .filter((box) => box.checked && !box.disabled)
      .map((box) => box.dataset.broadcastId);
  }

  function syncBroadcastSelection() {
    state.broadcastSelection = readBroadcastSelection();
  }

  async function sendBroadcast() {
    const text = $("broadcast-input").value.trim();
    if (!text) {
      showToast(t("broadcast-empty-prompt"));
      return;
    }
    // Re-checked at send time rather than trusted from when the dialog opened:
    // a session can enter a permission prompt while the operator is typing, and
    // the daemon would refuse it anyway.
    const ids = readBroadcastSelection().filter((id) =>
      isEligible(state.sessions.find((session) => session.id === id)),
    );
    if (!ids.length) {
      showToast(t("broadcast-none-eligible"));
      return;
    }
    try {
      const results = await invoke("broadcast_prompt", { ids, text });
      const { delivered, refused, total } = summarize(results);
      // Both numbers, always. "Sent" alone, when four of nine were skipped, is
      // the failure the per-session protocol exists to prevent.
      const message = refused
        ? `${t("broadcast-sent", { delivered, total })} · ${t("broadcast-refused", { count: refused })}`
        : t("broadcast-sent", { delivered, total });
      showToast(message, refused ? "" : "success");
      if (!refused) {
        $("broadcast-input").value = "";
        $("broadcast-dialog").close();
      } else {
        // Re-rendering after a partial refusal must preserve the boxes the
        // operator left checked, rather than restoring the open-time default.
        syncBroadcastSelection();
        renderBroadcast();
      }
    } catch (error) {
      showToast(t("broadcast-error", { error: String(error) }));
    }
  }

  return {
    openBroadcast,
    readBroadcastSelection,
    renderBroadcast,
    sendBroadcast,
    syncBroadcastSelection,
  };
}
