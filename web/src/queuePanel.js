/**
 * The per-session prompt queue dialog and its row action wiring.
 *
 * The fleet row only carries the count and pause state. The prompts are fetched
 * when the operator opens the dialog, so a large queue never rides on every
 * snapshot and every status update.
 */
export function createQueuePanel(deps) {
  const { $, state, invoke, showToast, t, escapeHtml, renderDataError } = deps;

  function queueGlyph(session) {
    const count = session.queued_prompts ?? 0;
    if (!count) return "≡";
    return count > 9 ? "9+" : String(count);
  }

  function queueTitle(session) {
    const count = session.queued_prompts ?? 0;
    if (session.queue_paused) {
      return t("queue-paused-title", { name: session.name, reason: t(`queue-pause-${session.queue_paused}`) });
    }
    return count ? t("queue-count-title", { name: session.name, count }) : t("action-queue", { name: session.name });
  }

  /**
   * The prompts waiting on one session.
   *
   * Fetched when the dialog opens rather than carried on every row: a session
   * is re-rendered on each status change, and the prompts can be a quarter of
   * a megabyte each.
   */
  async function openQueue(id) {
    state.queueSession = id;
    const dialog = $("queue-dialog");
    if (!dialog.open) dialog.showModal();
    await refreshQueue();
    $("queue-input").focus();
  }

  async function refreshQueue() {
    const id = state.queueSession;
    if (!id) return;
    try {
      state.queuePrompts = await invoke("queued_prompts", { id });
      state.queueError = null;
    } catch (error) {
      state.queuePrompts = [];
      state.queueError = String(error);
    }
    renderQueue();
  }

  function renderQueue() {
    const id = state.queueSession;
    const session = state.sessions.find((item) => item.id === id);
    $("queue-title").textContent = session ? t("queue-title", { name: session.name }) : t("queue-title-generic");
    const paused = session?.queue_paused ?? null;
    // Always stated, and always with the reason. "Paused" alone leaves the
    // operator guessing whether the agent is waiting on them or the queue is.
    $("queue-status").textContent = paused
      ? t("queue-paused-detail", { reason: t(`queue-pause-${paused}`) })
      : t("queue-running");
    $("queue-resume-button").hidden = !paused;
    $("queue-pause-button").hidden = Boolean(paused);

    if (state.queueError) {
      $("queue-status").textContent = t("queue-unavailable");
      $("queue-resume-button").hidden = true;
      $("queue-pause-button").hidden = true;
      renderDataError(
        $("queue-list"),
        t("queue-load-error", { error: state.queueError }),
        "queue",
        refreshQueue,
      );
      return;
    }

    if (!state.queuePrompts.length) {
      $("queue-list").innerHTML = `<p class="rollup-total">${escapeHtml(t("queue-empty"))}</p>`;
      return;
    }
    $("queue-list").innerHTML = state.queuePrompts
      .map((prompt, index) => {
        const position = t("queue-position", { position: index + 1 });
        return (
          `<li class="queue-row" data-prompt="${escapeHtml(String(prompt.id))}">` +
          `<span class="queue-position">${escapeHtml(position)}</span>` +
          `<textarea class="queue-text" rows="2" aria-label="${escapeHtml(position)}">` +
          `${escapeHtml(prompt.text)}</textarea>` +
          '<span class="queue-row-actions">' +
          `<button type="button" class="row-action" data-queue-action="up" ` +
          `title="${escapeHtml(t("queue-move-up"))}" aria-label="${escapeHtml(t("queue-move-up"))}">↑</button>` +
          `<button type="button" class="row-action" data-queue-action="down" ` +
          `title="${escapeHtml(t("queue-move-down"))}" aria-label="${escapeHtml(t("queue-move-down"))}">↓</button>` +
          `<button type="button" class="row-action" data-queue-action="save" ` +
          `title="${escapeHtml(t("queue-save"))}" aria-label="${escapeHtml(t("queue-save"))}">✓</button>` +
          `<button type="button" class="row-action row-action-danger" data-queue-action="remove" ` +
          `title="${escapeHtml(t("queue-withdraw"))}" aria-label="${escapeHtml(t("queue-withdraw"))}">×</button>` +
          "</span></li>"
        );
      })
      .join("");
    for (const button of $("queue-list").querySelectorAll("[data-queue-action]")) {
      button.addEventListener("click", () => void queueRowAction(button));
    }
  }

  async function queueRowAction(button) {
    const id = state.queueSession;
    const row = button.closest("[data-prompt]");
    const prompt = Number(row.dataset.prompt);
    const index = state.queuePrompts.findIndex((item) => item.id === prompt);
    const action = button.dataset.queueAction;
    try {
      if (action === "remove") await invoke("remove_queued_prompt", { id, prompt });
      if (action === "save") {
        await invoke("edit_queued_prompt", { id, prompt, text: row.querySelector(".queue-text").value });
        showToast(t("queue-saved"), "success");
      }
      if (action === "up" && index > 0) {
        await invoke("reorder_queued_prompt", { id, prompt, to: index - 1 });
      }
      if (action === "down" && index < state.queuePrompts.length - 1) {
        await invoke("reorder_queued_prompt", { id, prompt, to: index + 1 });
      }
    } catch (error) {
      // Usually a race: the prompt fired while the operator was deciding. The
      // backend names that case, so it is shown rather than swallowed.
      showToast(String(error));
    }
    await refreshQueue();
  }

  async function addQueuedPrompt() {
    const id = state.queueSession;
    const text = $("queue-input").value.trim();
    if (!text) {
      showToast(t("queue-empty-prompt"));
      return;
    }
    try {
      await invoke("enqueue_prompt", { id, text });
      $("queue-input").value = "";
    } catch (error) {
      showToast(String(error));
    }
    await refreshQueue();
  }

  function bindQueueEvents() {
    $("queue-add-button").addEventListener("click", () => void addQueuedPrompt());
    $("queue-pause-button").addEventListener("click", async () => {
      try {
        await invoke("pause_queue", { id: state.queueSession });
      } catch (error) {
        showToast(String(error));
      }
      await refreshQueue();
    });
    $("queue-resume-button").addEventListener("click", async () => {
      try {
        await invoke("resume_queue", { id: state.queueSession });
      } catch (error) {
        showToast(String(error));
      }
      await refreshQueue();
    });
  }

  return {
    bindEvents: bindQueueEvents,
    queueGlyph,
    queueTitle,
    openQueue,
    refreshQueue,
    renderQueue,
    queueRowAction,
    addQueuedPrompt,
  };
}
