/**
 * Fleet row creation, interaction, and incremental updates.
 *
 * The list coordinator decides which sessions are visible; this boundary keeps
 * row DOM identity, keyboard movement, action handling, and patch updates in
 * one place so reconciliation cannot drift from the initial markup.
 */
export function createFleetRowState(deps) {
  const {
    $,
    answerCountdownLabel,
    cost,
    costTitle,
    document,
    dwell,
    escapeHtml,
    focusSession,
    folderLabel,
    groupOf,
    isAttention,
    lastActivity,
    lifecycleDetail,
    lifecycleLabel,
    lifecycleTone,
    memory,
    memoryTitle,
    ports,
    queueGlyph,
    queueTitle,
    reconcileGroupChip,
    renderRow,
    renderRows,
    rowAction,
    showToast,
    state,
    STATUS_META,
    t,
    toolProgress,
  } = deps;

function createFleetRow(session) {
  const template = document.createElement("template");
  template.innerHTML = renderRow(session).trim();
  const row = template.content.firstElementChild;
  bindFleetRow(row);
  return row;
}

function bindFleetRow(row) {
  row.addEventListener("click", () => focusSession(row.dataset.id));
  row.addEventListener("keydown", (event) => {
    if (event.target !== row) return;
    if (["ArrowDown", "ArrowRight", "ArrowUp", "ArrowLeft", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      const horizontal = ["ArrowDown", "ArrowRight"].includes(event.key);
      const vertical = ["ArrowUp", "ArrowLeft"].includes(event.key);
      moveFleetRow(row, horizontal ? 1 : vertical ? -1 : event.key === "Home" ? "first" : "last");
      return;
    }
    if (!["Enter", " "].includes(event.key)) return;
    event.preventDefault();
    void focusSession(row.dataset.id);
  });
  for (const button of row.querySelectorAll("button[data-action]")) {
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      if (state.demoMode) {
        if (button.dataset.action === "focus") void focusSession(row.dataset.id);
        else if (button.dataset.action === "pin") {
          const session = state.sessions.find((item) => item.id === row.dataset.id);
          if (session) session.pinned = !session.pinned;
          renderRows();
        } else showToast(t("demo-read-only"), "success");
        return;
      }
      rowAction(button.dataset.action, row.dataset.id, row);
    });
  }
  const reply = row.querySelector("input[data-reply]");
  reply?.addEventListener("click", (event) => event.stopPropagation());
  reply?.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    if (state.demoMode) showToast(t("demo-read-only"), "success");
    else rowAction("reply", row.dataset.id, row);
  });
}

function moveFleetRow(row, direction) {
  const rows = Array.from($("fleet-list").querySelectorAll('[role="option"]')).filter((candidate) => !candidate.hidden);
  const index = rows.indexOf(row);
  if (index < 0 || rows.length < 2) return;
  const targetIndex = direction === "first"
    ? 0
    : direction === "last"
      ? rows.length - 1
      : Math.max(0, Math.min(rows.length - 1, index + direction));
  const target = rows[targetIndex];
  if (!target || target === row) return;
  row.tabIndex = -1;
  target.tabIndex = 0;
  target.focus({ preventScroll: false });
  void focusSession(target.dataset.id);
}

function updateFleetRow(row, session, position = 1, setSize = 1, rovingId = state.focused) {
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const label = lifecycleLabel(session);
  const detail = lifecycleDetail(session);
  const active = session.id === state.focused;
  const unread = Boolean(session.unread);
  const agentLabel = session.agent === "codex" ? "CX" : "CC";
  const model = session.model || "default";
  const effort = session.effort || "—";
  const repo = folderLabel(session.cwd);
  const branch = session.branch || "—";
  const progress = toolProgress(session.tool_progress);
  const restartCount = Number.isInteger(Number(session.restarts)) ? Number(session.restarts) : 0;
  const lastLine = lastActivity(session);
  const pinLabel = session.pinned ? t("action-unpin") : t("action-pin");
  const sessionLabel = `${session.name}, ${label}, ${repo}, ${branch}, ` +
    `${t("action-tool-progress")} ${progress}, ${restartCount} ${t("action-restart-count")}, ` +
    `${t("action-allocated-ports")} ${ports(session.ports)}`;

  row.className = `fleet-row${active ? " row-focused" : ""}${unread ? " row-unread" : ""}`;
  row.dataset.id = session.id;
  row.setAttribute("aria-label", sessionLabel);
  row.setAttribute("aria-posinset", String(position));
  row.setAttribute("aria-setsize", String(setSize));
  row.setAttribute("aria-selected", String(active));
  row.tabIndex = session.id === rovingId ? 0 : -1;

  const glyph = row.querySelector(".status-glyph");
  glyph.className = `status-glyph tone-${lifecycleTone(session, meta)}`;
  glyph.title = detail ? `${label} — ${detail}` : label;
  glyph.textContent = meta.glyph;

  row.querySelector(".row-name-text").textContent = session.name;
  const unreadDot = row.querySelector(".unread-dot");
  if (unread && !unreadDot) {
    const dot = document.createElement("span");
    dot.className = "unread-dot";
    dot.title = t("action-unread-attention");
    row.querySelector(".row-name").append(dot);
  } else if (!unread && unreadDot) {
    unreadDot.remove();
  }
  row.querySelector(".row-repo").textContent = repo;
  row.querySelector(".row-branch").textContent = branch;
  const statusLabelNode = row.querySelector(".row-status-label");
  statusLabelNode.textContent = label;
  statusLabelNode.title = detail || label;
  const portBadge = row.querySelector(".row-ports");
  portBadge.textContent = `${t("action-allocated-ports")} ${ports(session.ports)}`;
  reconcileGroupChip(
    row,
    state.groupBy === "none" ? "" : groupOf(session),
    t("row-group"),
  );

  const agentBadge = row.querySelector(".agent-badge");
  agentBadge.className = `agent-badge agent-${session.agent}`;
  agentBadge.title = session.agent === "codex" ? "Codex" : "Claude Code";
  agentBadge.setAttribute("aria-label", agentBadge.title);
  agentBadge.textContent = agentLabel;
  row.querySelector(".row-progress b").textContent = progress;
  row.querySelector(".row-restarts").textContent = `↻ ${restartCount}`;
  row.querySelector(".row-dwell > span").textContent = dwell(session.status_since);
  const lastLineElement = row.querySelector(".row-last-line");
  lastLineElement.title = lastLine;
  lastLineElement.textContent = lastLine;

  const pin = row.querySelector('[data-action="pin"]');
  pin.classList.toggle("row-action-active", session.pinned);
  pin.title = pinLabel;
  pin.setAttribute("aria-label", `${pinLabel} ${session.name}`);
  pin.textContent = session.pinned ? "◆" : "◇";
  const focus = row.querySelector('[data-action="focus"]');
  focus.setAttribute("aria-label", t("action-focus-session", { name: session.name }));
  const revive = row.querySelector('[data-action="revive"]');
  revive.hidden = !(session.status === "exited" && session.resume_id);
  revive.title = t("action-revive", { name: session.name });
  revive.setAttribute("aria-label", t("action-revive", { name: session.name }));
  const archive = row.querySelector('[data-action="archive"]');
  archive.hidden = session.status !== "exited";
  archive.setAttribute("aria-label", t("action-archive", { name: session.name }));
  const queueButton = row.querySelector('[data-action="queue"]');
  queueButton.textContent = queueGlyph(session);
  queueButton.classList.toggle("row-action-active", session.queued_prompts > 0);
  // A paused queue is the one state worth colouring: it means the queue has
  // stopped and is waiting for the operator, which is invisible from a count.
  queueButton.classList.toggle("row-action-warn", Boolean(session.queue_paused));
  const queueLabel = queueTitle(session);
  queueButton.title = queueLabel;
  queueButton.setAttribute("aria-label", queueLabel);
  const stop = row.querySelector('[data-action="kill"]');
  stop.hidden = session.status === "exited";
  const stopLabel = session.status === "queued" ? t("action-cancel-queued") : t("action-stop", { name: session.name });
  stop.title = stopLabel;
  stop.setAttribute("aria-label", stopLabel);

  const wideMeta = row.querySelector(".row-wide-meta");
  wideMeta.hidden = !state.wideMode;
  wideMeta.querySelector("[data-row-model]").textContent = model;
  wideMeta.querySelector("[data-row-effort]").textContent = effort;
  // The incremental path has to move every attribute the full render sets, or a
  // row that was patched rather than rebuilt keeps the previous session's
  // tooltip and tone.
  const costCell = wideMeta.querySelector("[data-row-cost]");
  costCell.textContent = cost(session.cost_usd);
  costCell.classList.toggle("row-budget-spent", Boolean(session.budget_exhausted));
  costCell.title = costTitle(session);
  // Conditional markup, so this cell is created and removed rather than written
  // to — the same shape the unread dot uses above. A row that becomes a team
  // lead mid-session gains the cell without being rebuilt, and one whose team
  // ends loses it rather than keeping the last names it had.
  const teamNames = Array.isArray(session.teammates) ? session.teammates : [];
  const existingTeam = wideMeta.querySelector("[data-row-team]");
  if (teamNames.length === 0) {
    existingTeam?.closest("span")?.remove();
  } else {
    const joined = teamNames.join(", ");
    const detail = t("team-explained", { names: joined, count: teamNames.length });
    if (existingTeam) {
      existingTeam.textContent = joined;
      existingTeam.title = detail;
    } else {
      const cell = document.createElement("span");
      const caption = document.createElement("small");
      caption.textContent = "TEAM";
      const value = document.createElement("b");
      value.dataset.rowTeam = "";
      value.textContent = joined;
      value.title = detail;
      cell.append(caption, value);
      wideMeta.append(cell);
    }
  }
  const memoryCell = wideMeta.querySelector("[data-row-memory]");
  memoryCell.textContent = memory(session.memory_bytes);
  memoryCell.classList.toggle("row-memory-limited", Boolean(session.memory_limited));
  memoryCell.title = memoryTitle(session);

  // A question the agent will answer for itself has a deadline. Saying how much
  // of it is left is the difference between "answer this" and "answer this in
  // the next twelve seconds or it decides for you".
  const countdown = row.querySelector(".row-answer-deadline");
  const countdownLabel = answerCountdownLabel(session);
  if (countdown) {
    countdown.hidden = !countdownLabel;
    countdown.textContent = countdownLabel;
    if (countdownLabel) countdown.title = t("answer-deadline-explained");
  }

  const reply = row.querySelector(".row-reply");
  const replyInput = row.querySelector("input[data-reply]");
  reply.hidden = !isAttention(session) && !replyInput.value;
  replyInput.setAttribute("aria-label", t("action-reply", { name: session.name }));
  row.querySelector(".row-reply-send").setAttribute("aria-label", t("action-send-reply", { name: session.name }));
}

  return { bindFleetRow, createFleetRow, moveFleetRow, updateFleetRow };
}
