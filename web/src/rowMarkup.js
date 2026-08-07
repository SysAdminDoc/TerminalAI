/**
 * The fleet row's markup.
 *
 * Extracted from `main.js` because these were the longest lines in the tree and
 * the most load-bearing: a typo inside a single-expression template is invisible
 * in a diff, and this one renders every row of the fleet.
 *
 * A factory rather than a plain export: the renderer reads eighteen helpers and
 * the live `state` object, and threading those through every call site would be
 * a worse trade than binding them once. `state` is held by reference, so
 * `wideMode` and `focused` are read when a row renders, not when this is built.
 */
export function createRowRenderer(deps) {
  const {
    STATUS_META,
    state,
    answerCountdownLabel,
    cost,
    dwell,
    escapeHtml,
    folderLabel,
    groupChip,
    isAttention,
    lastActivity,
    lifecycleDetail,
    lifecycleLabel,
    lifecycleTone,
    memory,
    ports,
    queueGlyph,
    t,
    toolProgress,
  } = deps;

  return function renderRow(session) {
    const meta = STATUS_META[session.status] ?? STATUS_META.exited;
    const label = lifecycleLabel(session);
    const detail = lifecycleDetail(session);
    const active = session.id === state.focused ? " row-focused" : "";
    const unread = session.unread ? " row-unread" : "";
    const agentLabel = session.agent === "codex" ? "CX" : "CC";
    const model = session.model || "default";
    const effort = session.effort || "—";
    const repo = folderLabel(session.cwd);
    const branch = session.branch || "—";
    const progress = toolProgress(session.tool_progress);
    const restartCount = Number.isInteger(Number(session.restarts)) ? Number(session.restarts) : 0;
    const lastLine = lastActivity(session);
    const pinLabel = session.pinned ? t("action-unpin") : t("action-pin");
    const reviveHidden = session.status === "exited" && session.resume_id ? "" : " hidden";
    const archiveHidden = session.status === "exited" ? "" : " hidden";
    const stopHidden = session.status === "exited" ? " hidden" : "";
    const stopLabel =
      session.status === "queued"
        ? t("action-cancel-queued")
        : t("action-stop", { name: session.name });
    const replyHidden = isAttention(session) ? "" : " hidden";
    const countdownLabel = answerCountdownLabel(session);
    const wideHidden = state.wideMode ? "" : " hidden";
    const portsLabel = ports(session.ports);
    const accessibleLabel =
      `${session.name}, ${label}, ${repo}, ${branch}, ${t("action-tool-progress")} ` +
      `${progress}, ${restartCount} ${t("action-restart-count")}, ` +
      `${t("action-allocated-ports")} ${portsLabel}`;
    // Each section is its own binding. A literal may be divided at any point
    // outside an interpolation, and an interpolation is always taken whole, so
    // the pieces concatenate back to the exact bytes the single-line template
    // produced -- proven by rendering fixtures before the split and comparing
    // them after, not by reading the diff.
    const articleOpen =
      `<article class="fleet-row${escapeHtml(active)}${escapeHtml(unread)}" data-id="` +
      `${escapeHtml(session.id)}" role="option" tabindex="-1" aria-posinset="1" ` +
      `aria-setsize="1" aria-selected="false" aria-keyshortcuts="Enter Space ArrowUp ` +
      `ArrowDown Home End" aria-label="${escapeHtml(accessibleLabel)}">`;

    const unreadDot = session.unread
      ? `<span class="unread-dot" title="${escapeHtml(t("action-unread-attention"))}"></span>`
      : "";

    const identity =
      `<div class="row-identity"><span class="status-glyph tone-` +
      `${escapeHtml(lifecycleTone(session, meta))}" title="` +
      `${escapeHtml(detail ? `${label} — ${detail}` : label)}" aria-hidden="true">` +
      `${meta.glyph}</span><div class="row-name-wrap"><div class="row-name"><span class="` +
      `row-name-text">${escapeHtml(session.name)}</span>` +
      `${unreadDot}` +
      `</div><div class="row-folder"><span class="row-repo" title="` +
      `${escapeHtml(t("action-repository"))}">${escapeHtml(repo)}</span><span class="` +
      `row-branch" title="${escapeHtml(t("action-branch"))}">${escapeHtml(branch)}</span>` +
      `<span class="row-status-label" title="${escapeHtml(detail || label)}">` +
      `${escapeHtml(label)}</span>${groupChip(session)}<span class="row-ports" title="` +
      `${escapeHtml(t("action-allocated-ports"))}">${escapeHtml(t("action-allocated-ports"))}` +
      ` ${escapeHtml(portsLabel)}</span></div></div></div>`;

    const metrics =
      `<div class="row-metrics"><span class="agent-badge agent-${escapeHtml(session.agent)}" ` +
      `title="${session.agent === "codex" ? "Codex" : "Claude Code"}" aria-label="` +
      `${session.agent === "codex" ? "Codex" : "Claude Code"}">${agentLabel}</span><span ` +
      `class="row-progress" title="${escapeHtml(t("action-tool-progress"))}"><small>` +
      `PROG</small><b>${escapeHtml(progress)}</b></span><span class="row-restarts" title="` +
      `${escapeHtml(t("action-restart-count"))}">↻ ${restartCount}</span></div>`;

    const dwellCell =
      `<div class="row-dwell"><span title="${escapeHtml(t("dwell-explained"))}">` +
      `${dwell(session.status_since)}</span><span class="row-answer-deadline" title="` +
      `${escapeHtml(t("answer-deadline-explained"))}"${countdownLabel ? "" : " hidden"}>` +
      `${escapeHtml(countdownLabel)}</span><small class="row-last-line" title="` +
      `${escapeHtml(lastLine)}">${escapeHtml(lastLine)}</small></div>`;

    const actions =
      `<div class="row-actions"><button type="button" data-action="pin" class="row-action ` +
      `${session.pinned ? "row-action-active" : ""}" title="${escapeHtml(pinLabel)}" ` +
      `aria-label="${escapeHtml(pinLabel)} ${escapeHtml(session.name)}">` +
      `${session.pinned ? "◆" : "◇"}</button><button type="button" data-action="focus" ` +
      `class="row-action" title="${escapeHtml(t("action-focus-terminal"))}" aria-label="` +
      `${escapeHtml(t("action-focus-session", { name: session.name }))}">↗</button><button ` +
      `type="button" data-action="revive" class="row-action" title="` +
      `${escapeHtml(t("action-revive", { name: session.name }))}" aria-label="` +
      `${escapeHtml(t("action-revive", { name: session.name }))}"${reviveHidden}>↻</button>` +
      `<button type="button" data-action="archive" class="row-action" title="` +
      `${escapeHtml(t("action-archive-stopped"))}" aria-label="` +
      `${escapeHtml(t("action-archive", { name: session.name }))}"${archiveHidden}>▣</button>` +
      `<button type="button" data-action="queue" class="row-action row-action-queue" title="` +
      `${escapeHtml(t("action-queue", { name: session.name }))}" aria-label="` +
      `${escapeHtml(t("action-queue", { name: session.name }))}">` +
      `${escapeHtml(queueGlyph(session))}</button><button type="button" data-action="kill" ` +
      `class="row-action row-action-danger" title="${escapeHtml(stopLabel)}" aria-label="` +
      `${escapeHtml(stopLabel)}"${stopHidden}>×</button></div>`;

    const wideMeta =
      `<div class="row-wide-meta"${wideHidden}><span><small>MODEL</small><b data-row-model>` +
      `${escapeHtml(model)}</b></span><span><small>EFFORT</small><b data-row-effort>` +
      `${escapeHtml(effort)}</b></span><span><small>COST</small><b data-row-cost>` +
      `${escapeHtml(cost(session.cost_usd))}</b></span><span><small>MEM</small><b ` +
      `data-row-memory${session.memory_limited ? ' class="row-memory-limited"' : ""} title="` +
      `${escapeHtml(session.memory_limited ? t("memory-limited-explained") : t("memory-explained"))}` +
      `">${escapeHtml(memory(session.memory_bytes))}</b></span></div>`;

    const reply =
      `<div class="row-reply"${replyHidden}><input data-reply type="text" maxlength="500" ` +
      `placeholder="${escapeHtml(t("action-reply", { name: session.name }))}" aria-label="` +
      `${escapeHtml(t("action-reply", { name: session.name }))}" /><button type="button" ` +
      `data-action="reply" class="row-reply-send" title="` +
      `${escapeHtml(t("button-send-reply"))}" aria-label="` +
      `${escapeHtml(t("action-send-reply", { name: session.name }))}">↵</button></div>`;

    const sections = [identity, metrics, dwellCell, actions, wideMeta, reply];
    // The template put a newline and four spaces between sections, and a
    // newline and two before the closing tag. Those are markup, not source
    // indentation, so they are reproduced literally rather than re-indented.
    const gap = "\n    ";
    return `${articleOpen}${gap}${sections.join(gap)}\n  </article>`;
  };
}
