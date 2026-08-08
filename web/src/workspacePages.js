/**
 * Workspace utility pages: projects, saved layouts, fleet search, history,
 * and stale worktree cleanup.
 *
 * These pages all open from the persistent shell and are intentionally kept
 * together so the entry module remains an event coordinator rather than a
 * second implementation of every dialog.
 */
export function createWorkspacePages(deps) {
  const {
    $,
    state,
    invoke,
    t,
    escapeHtml,
    renderDataError,
    openWorkspacePage,
    sortProjects,
    hasOpenWork,
    summarizeProjects,
    openItemsCell,
    stalenessLabel,
    renderWorkingSet,
    renderRestoreOutcomes,
    summarizeRestore,
    renderSearchResults,
    searchSummary,
    renderSessionHistory,
    renderWorktrees,
    focusSession,
    openLauncher,
    schedulePreview,
    loadProjectTemplates,
    loadProjectRoots,
    loadStoredPrompts,
    refreshWorkRun,
    refreshWorkSchedule,
    showToast,
    pendingApprovals,
    renderApprovals,
    requestLine,
    waitingSince,
    relativeDwell,
    systemTimeMs,
  } = deps;

function renderProjects() {
  if (state.projectsError) {
    $("projects-coverage").textContent = t("projects-unavailable");
    renderDataError(
      $("projects-body"),
      t("projects-load-error", { error: state.projectsError }),
      "projects",
      openProjects,
    );
    return;
  }
  const openOnly = $("projects-open-only").checked;
  const all = state.scannedProjects;
  const rows = sortProjects(openOnly ? all.filter((item) => hasOpenWork(item.roadmap)) : all);
  const { withWork, unknown, total } = summarizeProjects(all);
  $("projects-coverage").textContent = total
    ? t("projects-summary", { withWork, total, unknown })
    : t("projects-none-registered");

  if (!rows.length) {
    $("projects-body").innerHTML = `<p class="rollup-total">${escapeHtml(
      total ? t("projects-none-matching") : t("projects-none-registered"),
    )}</p>`;
    return;
  }
  $("projects-body").innerHTML = `
    <table class="rollup-table projects-table">
      <thead><tr>
        <th>${escapeHtml(t("projects-column-project"))}</th>
        <th class="rollup-number">${escapeHtml(t("projects-column-open"))}</th>
        <th>${escapeHtml(t("projects-column-touched"))}</th>
        <th>${escapeHtml(t("projects-column-next"))}</th>
        <th></th>
      </tr></thead>
      <tbody>${rows
        .map((item) => {
          const cell = openItemsCell(item.roadmap, t);
          const touched = stalenessLabel(item.roadmap, t) ?? "—";
          const next = item.roadmap?.next_item ?? "";
          // Each cell is built separately so no interpolation has to wrap, and
          // every value is escaped at the point it lands — including inside an
          // attribute, which `contentSecurity.test.mjs` checks uniformly.
          const cells = [
            `<th scope="row" title="${escapeHtml(item.path)}">${escapeHtml(item.name)}</th>`,
            `<td class="rollup-number${cell.known ? "" : " rollup-unpriced"}">${escapeHtml(cell.text)}</td>`,
            `<td>${escapeHtml(touched)}</td>`,
            `<td class="projects-next">${escapeHtml(next)}</td>`,
            `<td><button type="button" class="button button-quiet" data-launch-project="${escapeHtml(item.path)}">` +
              `${escapeHtml(t("projects-launch"))}</button></td>`,
          ];
          return `<tr>${cells.join("")}</tr>`;
        })
        .join("")}</tbody>
    </table>`;
  for (const button of $("projects-body").querySelectorAll("[data-launch-project]")) {
    button.addEventListener("click", () => {
      $("projects-dialog").close();
      openLauncher();
      $("cwd-input").value = button.dataset.launchProject;
      schedulePreview();
      void loadProjectTemplates();
    });
  }
}

/// Show every session waiting on a decision.
///
/// A view over the snapshot the window already holds — a blocked session
/// carries its own pending request — so there is no separate poll and nothing
/// here can disagree with the rows behind it.
function openApprovals() {
  const dialog = $("approvals-dialog");
  openWorkspacePage(dialog);
  renderGuarded(
    $("approvals-body"),
    t("approvals-render-error"),
    "openApprovals",
    openApprovals,
    renderApprovalInbox,
  );
}

function renderApprovalInbox() {
  const dialog = $("approvals-dialog");
  if (!dialog.open) return;
  const waiting = pendingApprovals(state.sessions);
  $("approvals-count").textContent = t("approvals-count", { count: waiting.length });
  const railApprovalCount = $("rail-approval-count");
  if (railApprovalCount) {
    railApprovalCount.textContent = String(waiting.length);
    railApprovalCount.hidden = waiting.length === 0;
  }
  const body = $("approvals-body");
  // The inbox follows the fleet, so it re-renders whenever any session
  // changes — and a session unrelated to this one changing must not wipe an
  // answer being typed. Skipped entirely when nothing the inbox shows has
  // moved, and typed text is carried across when it has.
  const signature = waiting
    .map((session) => `${session.id}:${requestLine(session, t)}`)
    .join("|");
  if (body.dataset.signature === signature) return;
  const typed = new Map(
    [...body.querySelectorAll("[data-approval-reply]")]
      .filter((element) => element.value)
      .map((element) => [element.dataset.approvalReply, element.value]),
  );
  body.dataset.signature = signature;
  body.innerHTML = renderApprovals(waiting, {
    escape: escapeHtml,
    translate: t,
    dwell: (session) => relativeDwell(waitingSince(session) ?? systemTimeMs(session.status_since)),
  });
  for (const [id, value] of typed) {
    const input = [...body.querySelectorAll("[data-approval-reply]")].find(
      (element) => element.dataset.approvalReply === id,
    );
    if (input) input.value = value;
  }
  for (const button of body.querySelectorAll("[data-approval-focus]")) {
    button.addEventListener("click", () => {
      dialog.close();
      void focusSession(button.dataset.approvalFocus);
    });
  }
  for (const button of body.querySelectorAll("[data-approval-send]")) {
    button.addEventListener("click", () => void sendApproval(button.dataset.approvalSend));
  }
  for (const input of body.querySelectorAll("[data-approval-reply]")) {
    input.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      void sendApproval(input.dataset.approvalReply);
    });
  }
}

/// Send the operator's answer to the session that asked.
///
/// The same bracketed-paste write the fleet row's reply box uses, because it is
/// the same act: typing at that session's prompt. Nothing is auto-approved and
/// no universal "yes" is invented — what the agent accepts is its own prompt's
/// vocabulary, and guessing it would be answering on the operator's behalf.
async function sendApproval(id) {
  // Walked rather than selected: a session id in a selector string is the same
  // shape of mistake as one in markup, even though the escaper would differ.
  const input = [...$("approvals-body").querySelectorAll("[data-approval-reply]")].find(
    (element) => element.dataset.approvalReply === id,
  );
  const answer = input?.value.trim();
  if (!answer) return;
  try {
    await invoke("write_session", { id, data: `[200~${answer}[201~
` });
    await invoke("mark_read", { id });
    input.value = "";
    showToast(t("approvals-sent"), "success");
  } catch (error) {
    showToast(String(error));
  }
}

/// Show the saved layouts.
async function openWorkingSets() {
  const dialog = $("working-sets-dialog");
  openWorkspacePage(dialog);
  await refreshWorkingSets();
}

async function refreshWorkingSets() {
  const body = $("working-sets-body");
  try {
    const sets = await invoke("list_working_sets");
    $("working-sets-count").textContent = t("working-sets-count", { count: sets.length });
    body.innerHTML = sets.length
      ? sets.map((set) => renderWorkingSet(set, { escape: escapeHtml, translate: t })).join("")
      : `<p class="rollup-total">${escapeHtml(t("working-sets-empty"))}</p>`;
  } catch (error) {
    $("working-sets-count").textContent = "";
    body.innerHTML = `<p class="rollup-total">${escapeHtml(String(error))}</p>`;
    return;
  }
  for (const button of body.querySelectorAll("[data-restore-set]")) {
    button.addEventListener("click", () => void restoreWorkingSet(button.dataset.restoreSet));
  }
  for (const button of body.querySelectorAll("[data-delete-set]")) {
    button.addEventListener("click", async () => {
      try {
        await invoke("delete_working_set", { name: button.dataset.deleteSet });
        await refreshWorkingSets();
      } catch (error) {
        showToast(String(error));
      }
    });
  }
}

/// Relaunch a layout and show, per session, what the fleet decided.
///
/// The outcomes are rendered in place rather than toasted: a restore of twelve
/// sessions can have several distinct refusals, and a toast holds one line for
/// four seconds.
async function restoreWorkingSet(name) {
  // Found by walking the elements rather than by building a selector string:
  // a layout name is operator input, and interpolating it into a selector is
  // the same shape of mistake as interpolating it into markup even though the
  // escaper would differ.
  const list = [...$("working-sets-body").querySelectorAll("[data-outcomes-for]")].find(
    (element) => element.dataset.outcomesFor === name,
  );
  if (list) list.innerHTML = `<li>${escapeHtml(t("loading"))}</li>`;
  try {
    const outcomes = await invoke("restore_working_set", { name });
    if (list) {
      list.innerHTML = renderRestoreOutcomes(outcomes, { escape: escapeHtml, translate: t });
    }
    showToast(t("working-sets-restored", summarizeRestore(outcomes)), "success");
  } catch (error) {
    if (list) list.innerHTML = "";
    showToast(String(error));
  }
}

/// Ask the daemon which sessions printed a string, and how many times.
///
/// Find-in-pane answers the same question for the one attached renderer; this
/// is the reason the disk tier exists at all — the other twenty-nine sessions
/// have no renderer, and until now their output could only be read by focusing
/// each one in turn.
async function runFleetSearch() {
  const needle = $("fleet-search-input").value.trim();
  const body = $("fleet-search-body");
  const count = $("fleet-search-count");
  if (needle.length < 2) {
    count.textContent = "";
    body.innerHTML = `<p class="rollup-total">${escapeHtml(t("fleet-search-too-short"))}</p>`;
    return;
  }
  const button = $("fleet-search-run");
  button.disabled = true;
  body.innerHTML = `<p class="rollup-total">${escapeHtml(t("loading"))}</p>`;
  try {
    const matches = await invoke("search_fleet", {
      needle,
      caseSensitive: $("fleet-search-case").checked,
    });
    count.textContent = t("fleet-search-summary", searchSummary(matches));
    body.innerHTML = renderSearchResults(matches, {
      escape: escapeHtml,
      translate: t,
      needle,
    });
    for (const element of body.querySelectorAll("[data-search-focus]")) {
      element.addEventListener("click", () => {
        $("search-dialog").close();
        void focusSession(element.dataset.searchFocus);
      });
    }
  } catch (error) {
    // A retry, not just the text. This one has a real backend behind it, so a
    // failure is often transient and re-running is exactly what the operator
    // would do -- and had to do by retyping.
    count.textContent = "";
    renderDataError(
      body,
      t("fleet-search-error", { error: String(error) }),
      "runFleetSearch",
      runFleetSearch,
    );
  } finally {
    button.disabled = false;
  }
}

async function openSessionHistory() {
  const dialog = $("history-dialog");
  openWorkspacePage(dialog);
  $("history-body").innerHTML = `<p class="rollup-total">${escapeHtml(t("loading"))}</p>`;
  let archives = [];
  try {
    archives = await invoke("session_history");
  } catch (error) {
    $("history-count").textContent = "";
    $("history-body").innerHTML =
      `<p class="rollup-total">${escapeHtml(t("session-history-error", { error: String(error) }))}</p>`;
    return;
  }
  $("history-count").textContent = t("session-history-count", { count: archives.length });
  $("history-body").innerHTML = renderSessionHistory(archives, {
    escape: escapeHtml,
    translate: t,
    formatTime: (ms) => new Date(ms).toLocaleString(),
  });
  await refreshWorktrees();
  for (const button of $("history-body").querySelectorAll("[data-relaunch]")) {
    button.addEventListener("click", () => {
      const archive = archives.find((item) => item.id === button.dataset.relaunch);
      if (!archive) return;
      dialog.close();
      openLauncher();
      // Only what the archive actually holds. The command is kept as text, so
      // restoring a model or a sandbox from it would mean parsing an argv this
      // record never promised to keep parseable.
      $("agent-input").value = archive.agent;
      $("name-input").value = archive.name ?? "";
      $("cwd-input").value = archive.cwd ?? "";
      schedulePreview();
      void loadProjectTemplates();
    });
  }
}

/** Survey leftover checkouts inside the history dialog, which is where a
 * finished session's leavings belong. */
async function refreshWorktrees() {
  let worktrees = [];
  try {
    worktrees = await invoke("stale_worktrees");
  } catch (error) {
    $("worktrees-count").textContent = "";
    $("worktrees-body").innerHTML =
      `<p class="rollup-total">${escapeHtml(t("worktrees-error", { error: String(error) }))}</p>`;
    return;
  }
  $("worktrees-count").textContent = t("worktrees-count", { count: worktrees.length });
  $("worktrees-body").innerHTML = renderWorktrees(worktrees, { escape: escapeHtml, translate: t });
  for (const button of $("worktrees-body").querySelectorAll("[data-reap]")) {
    button.addEventListener("click", async () => {
      const stale = worktrees[Number(button.dataset.reap)];
      if (!stale) return;
      button.disabled = true;
      try {
        await invoke("reap_worktree", { stale });
        showToast(t("worktrees-removed"), "success");
      } catch (error) {
        // The core refuses unmerged and unknown states too, so a refusal that
        // reaches here is worth reading rather than retrying.
        showToast(String(error));
        button.disabled = false;
        return;
      }
      await refreshWorktrees();
    });
  }
}

async function openProjects() {
  const dialog = $("projects-dialog");
  openWorkspacePage(dialog);
  // Scanning reads a file per project, so the dialog opens first and fills in
  // rather than blocking on a few hundred reads before anything appears.
  $("projects-body").innerHTML = `<p class="rollup-total">${escapeHtml(t("loading"))}</p>`;
  try {
    state.scannedProjects = await invoke("scan_projects");
    state.projectsError = null;
  } catch (error) {
    state.scannedProjects = [];
    state.projectsError = String(error);
  }
  renderProjects();
  await Promise.all([loadProjectRoots(), loadStoredPrompts()]);
  await refreshWorkRun();
  await refreshWorkSchedule();
}

/** The queue button's glyph: the count when there is one, an outline when not. */

  return {
    renderProjects,
    openApprovals,
    renderApprovalInbox,
    openWorkingSets,
    refreshWorkingSets,
    restoreWorkingSet,
    runFleetSearch,
    openSessionHistory,
    refreshWorktrees,
    openProjects,
  };
}
