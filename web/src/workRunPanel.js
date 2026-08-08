/**
 * The work run: the prompt library, the run across many projects, and the
 * schedule that repeats it.
 *
 * Split out of `main.js` following `rowMarkup.js` and `terminalPane.js`. This
 * is one dialog's worth of behaviour — the only surface in the app that starts
 * agents in repositories the operator is not watching — and it was fused into a
 * scope where every edit risked every feature.
 *
 * A factory rather than plain exports, for the same reason the other two are:
 * the code reads the live `state` object and a handful of helpers `main.js`
 * owns. `state` is held by reference, so `state.workRun`, `state.workSchedule`
 * and `state.storedPrompts` are the same values the rest of the app sees.
 *
 * The moved code is unchanged. Only the indentation and this wrapper are new.
 */
import { REPEAT_CHOICES, scheduleStatus } from "./workSchedule.js";

export function createWorkRunPanel(deps) {
  const {
    $,
    state,
    invoke,
    showToast,
    t,
    escapeHtml,
    hasOpenWork,
    openWorkspacePage = (dialog) => {
      if (!dialog.open) dialog.showModal();
    },
  } = deps;

  const WORK_STATE_LABEL = {
    pending: "work-state-pending",
    running: "work-state-running",
    done: "work-state-done",
    failed: "work-state-failed",
    skipped: "work-state-skipped",
    flagged: "work-state-flagged",
    // Its own category, not a failure: nothing went wrong, the fleet was busy for
    // longer than the work was worth.
    expired: "work-state-expired",
  };

  /**
   * The run in progress, if there is one.
   *
   * Every category is shown, including the ones that did nothing. A run over
   * forty projects reporting only "done" is one the operator has to audit by
   * hand, which is the work they were trying to avoid.
   */
  function renderWorkRun() {
    const run = state.workRun;
    const body = $("work-run-body");
    body.hidden = !run;
    $("work-pause-button").hidden = !run || run.paused;
    $("work-resume-button").hidden = !run || !run.paused;
    $("work-clear-button").hidden = !run;
    if (!run) return;

    const counts = run.entries.reduce((totals, entry) => {
      const kind = entry.state.kind;
      totals[kind] = (totals[kind] ?? 0) + 1;
      return totals;
    }, {});
    const summary = t("work-outcome", {
      done: counts.done ?? 0,
      running: counts.running ?? 0,
      pending: counts.pending ?? 0,
      flagged: counts.flagged ?? 0,
      failed: counts.failed ?? 0,
      skipped: counts.skipped ?? 0,
      expired: counts.expired ?? 0,
    });
    const rows = run.entries
      .map((entry) => {
        const kind = entry.state.kind;
        const label = t(WORK_STATE_LABEL[kind] ?? "work-state-pending");
        const detail = workEntryDetail(entry);
        // Only a flagged entry offers a decision; everything else is a report.
        const actions =
          kind === "flagged"
            ? "<span>" +
              `<button type="button" class="button button-quiet" data-work-approve="${escapeHtml(entry.project)}">` +
              `${escapeHtml(t("work-approve"))}</button>` +
              `<button type="button" class="button button-quiet" data-work-skip="${escapeHtml(entry.project)}">` +
              `${escapeHtml(t("work-skip"))}</button></span>`
            : "<span></span>";
        return (
          `<div class="work-entry work-entry-${escapeHtml(kind)}">` +
          `<span title="${escapeHtml(entry.project)}">${escapeHtml(entry.name)}</span>` +
          `<span class="work-entry-state" title="${escapeHtml(detail)}">${escapeHtml(label)}</span>` +
          `${actions}</div>`
        );
      })
      .join("");
    const heading = run.paused ? `${t("work-run-paused")} · ${summary}` : summary;
    body.innerHTML = `<p class="rollup-total">${escapeHtml(heading)}</p>${rows}`;
    for (const button of body.querySelectorAll("[data-work-approve]")) {
      button.addEventListener("click", () => {
        void workEntryAction("approve_flagged_project", button.dataset.workApprove);
      });
    }
    for (const button of body.querySelectorAll("[data-work-skip]")) {
      button.addEventListener("click", () => void workEntryAction("skip_work_project", button.dataset.workSkip));
    }
  }

  /** Why an entry is in the state it is, for the row's tooltip. */
  function workEntryDetail(entry) {
    const state = entry.state;
    if (state.kind === "failed") return state.detail ?? "";
    if (state.kind === "flagged") {
      const tree = state.tree ?? {};
      if (tree.kind === "dirty") return t("work-dirty-detail", { count: tree.files?.length ?? 0 });
      if (tree.kind === "unknown") return `${t("work-tree-unknown")}: ${tree.detail ?? ""}`;
    }
    if (state.kind === "running" || state.kind === "done") return state.session ?? "";
    if (state.kind === "expired") {
      return t("work-expired-detail", { minutes: Math.round(Number(state.waited_seconds ?? 0) / 60) });
    }
    return "";
  }

  async function workEntryAction(command, path) {
    try {
      await invoke(command, { path });
    } catch (error) {
      showToast(String(error));
    }
    await refreshWorkRun();
  }

  async function setWorkRunPaused(paused) {
    try {
      await invoke("set_work_run_paused", { paused });
    } catch (error) {
      showToast(String(error));
    }
    await refreshWorkRun();
  }

  async function refreshWorkRun() {
    try {
      state.workRun = await invoke("work_run");
    } catch (error) {
      state.workRun = null;
      showToast(String(error));
    }
    renderWorkRun();
  }

  /** The cadence dropdown, built once from the choices the backend accepts. */
  function fillRepeatChoices() {
    const select = $("work-repeat-select");
    if (select.options.length) return;
    const off = document.createElement("option");
    off.value = "";
    off.textContent = t("work-repeat-off");
    select.append(off);
    for (const choice of REPEAT_CHOICES) {
      const option = document.createElement("option");
      option.value = String(choice.seconds);
      option.textContent = t(choice.key, choice.args);
      select.append(option);
    }
  }

  /**
   * What the standing schedule is about to do, and what its last firing did.
   *
   * Including the firings that did nothing: a schedule the operator was not
   * present for is only worth having if it can say what happened while they were
   * away, and one that reports only its successes is the same as one that reports
   * nothing.
   */
  function renderWorkSchedule() {
    fillRepeatChoices();
    const schedule = state.workSchedule;
    const select = $("work-repeat-select");
    const wanted = schedule ? String(schedule.interval_seconds) : "";
    // Only when it differs: assigning on every render would fight the operator
    // mid-choice on a re-render triggered by an unrelated session.
    if (select.value !== wanted) select.value = wanted;
    $("work-schedule-pause-button").hidden = !schedule || schedule.paused;
    $("work-schedule-resume-button").hidden = !schedule || !schedule.paused;
    const line = $("work-schedule-status");
    const parts = scheduleStatus(schedule, Date.now());
    line.hidden = !parts?.length;
    line.textContent = (parts ?? []).map((part) => t(part.key, part.args)).join(" · ");
  }

  async function refreshWorkSchedule() {
    try {
      state.workSchedule = await invoke("work_schedule");
    } catch (error) {
      state.workSchedule = null;
      showToast(String(error));
    }
    renderWorkSchedule();
  }

  /**
   * Stand up, replace or remove the repeating run.
   *
   * The projects are the ones currently listed, exactly as the run button uses
   * them: the filter above the table is how the operator says which they mean,
   * and a schedule that quietly targeted a different set than the button beside
   * it would be the worst kind of surprise to leave running unattended.
   */
  async function setWorkSchedule() {
    const seconds = Number($("work-repeat-select").value);
    try {
      if (!seconds) {
        await invoke("clear_work_schedule");
        state.workSchedule = null;
        showToast(t("work-schedule-cleared"), "success");
      } else {
        const prompt = $("work-prompt-select").value;
        if (!prompt) return;
        state.workSchedule = await invoke("set_work_schedule", {
          prompt,
          projects: listedProjects().map((item) => item.path),
          intervalSeconds: seconds,
        });
        showToast(t("work-schedule-set"), "success");
      }
    } catch (error) {
      showToast(String(error));
      await refreshWorkSchedule();
      return;
    }
    renderWorkSchedule();
  }

  async function setWorkSchedulePaused(paused) {
    try {
      state.workSchedule = await invoke("set_work_schedule_paused", { paused });
    } catch (error) {
      showToast(String(error));
    }
    renderWorkSchedule();
  }

  function renderPromptLibrary() {
    const list = $("stored-prompt-list");
    if (state.storedPromptsError) {
      $("prompt-library-count").textContent = t("prompt-library-unavailable");
      renderDataError(
        list,
        t("prompt-library-load-error", { error: state.storedPromptsError }),
        "prompt-library",
        loadStoredPrompts,
      );
      return;
    }
    $("prompt-library-count").textContent = t("prompt-library-count", {
      count: state.storedPrompts.length,
    });
    if (!state.storedPrompts.length) {
      list.innerHTML =
        '<p class="rollup-total">' + escapeHtml(t("prompt-library-empty")) + "</p>";
      return;
    }
    list.innerHTML = state.storedPrompts
      .map((prompt) => {
        const name = String(prompt.name ?? "");
        const selectLabel = t("prompt-select", { name });
        const deleteLabel = t("prompt-delete", { name });
        const source = prompt.source ? t("prompt-source-seeded") : t("prompt-source-local");
        const active = name === state.activeStoredPrompt;
        return '<div class="stored-prompt-row" role="listitem">' +
          '<button type="button" class="stored-prompt-select" data-prompt-select="' +
          escapeHtml(name) +
          '" aria-pressed="' +
          String(active) +
          '" aria-label="' +
          escapeHtml(selectLabel) +
          '"><span>' +
          escapeHtml(name) +
          "</span><small>" +
          escapeHtml(source) +
          '</small></button><button type="button" class="row-action row-action-danger" data-prompt-delete="' +
          escapeHtml(name) +
          '" title="' +
          escapeHtml(deleteLabel) +
          '" aria-label="' +
          escapeHtml(deleteLabel) +
          '">×</button></div>';
      })
      .join("");
  }

  function newStoredPrompt() {
    state.activeStoredPrompt = null;
    $("stored-prompt-name").value = "";
    $("stored-prompt-text").value = "";
    renderPromptLibrary();
  }

  function selectStoredPrompt(name) {
    const prompt = state.storedPrompts.find((entry) => entry.name === name);
    if (!prompt) {
      newStoredPrompt();
      return;
    }
    state.activeStoredPrompt = prompt.name;
    $("stored-prompt-name").value = prompt.name;
    $("stored-prompt-text").value = prompt.text;
    renderPromptLibrary();
  }

  async function saveStoredPrompt() {
    const name = $("stored-prompt-name").value.trim();
    const text = $("stored-prompt-text").value;
    if (!name) {
      showToast(t("prompt-name-required"));
      $("stored-prompt-name").focus();
      return;
    }
    if (!text.trim()) {
      showToast(t("prompt-text-required"));
      $("stored-prompt-text").focus();
      return;
    }
    const previous = state.activeStoredPrompt;
    try {
      await invoke("save_stored_prompt", {
        prompt: { name, text, source: null },
      });
      if (previous && previous !== name) {
        await invoke("delete_stored_prompt", { name: previous });
      }
      state.activeStoredPrompt = name;
      await loadStoredPrompts();
      selectStoredPrompt(name);
      showToast(t("prompt-saved", { name }), "success");
    } catch (error) {
      showToast(String(error));
    }
  }

  async function deleteStoredPrompt(name) {
    if (!name) return;
    try {
      const removed = await invoke("delete_stored_prompt", { name });
      if (removed && state.activeStoredPrompt === name) newStoredPrompt();
      await loadStoredPrompts();
      showToast(
        removed ? t("prompt-deleted", { name }) : t("prompt-not-found", { name }),
        removed ? "success" : "",
      );
    } catch (error) {
      showToast(String(error));
    }
  }

  async function loadStoredPrompts() {
    try {
      state.storedPrompts = await invoke("list_stored_prompts");
      state.storedPromptsError = null;
    } catch (error) {
      state.storedPrompts = [];
      state.storedPromptsError = String(error);
    }
    const select = $("work-prompt-select");
    const selected = select.value;
    select.innerHTML = state.storedPrompts
      .map((prompt) => '<option value="' + escapeHtml(prompt.name) + '">' + escapeHtml(prompt.name) + "</option>")
      .join("");
    // With nothing stored there is nothing to run, and a button that always
    // errors is worse than one that is plainly unavailable.
    const empty = state.storedPrompts.length === 0;
    $("work-start-button").disabled = empty;
    select.disabled = empty;
    if (!empty && state.storedPrompts.some((prompt) => prompt.name === selected)) {
      select.value = selected;
    }
    if (empty) {
      select.innerHTML = '<option value="">' + escapeHtml(t("work-no-prompts")) + "</option>";
    }
    if (
      state.activeStoredPrompt &&
      !state.storedPrompts.some((prompt) => prompt.name === state.activeStoredPrompt)
    ) {
      newStoredPrompt();
    } else {
      renderPromptLibrary();
    }
  }

  function openPromptLibrary() {
    const dialog = $("prompt-dialog");
    openWorkspacePage(dialog);
    void loadStoredPrompts();
  }

  /**
   * Run the chosen prompt across the projects currently listed.
   *
   * Deliberately the *listed* projects rather than all known ones: the filter
   * above the table is how the operator says which they mean, and a button that
   * ignored it would launch agents in repositories they had just filtered out.
   */
  function listedProjects() {
    const openOnly = $("projects-open-only").checked;
    return openOnly
      ? state.scannedProjects.filter((item) => hasOpenWork(item.roadmap))
      : state.scannedProjects;
  }

  async function startWorkRun() {
    const prompt = $("work-prompt-select").value;
    if (!prompt) return;
    const listed = listedProjects();
    if (!listed.length) {
      showToast(t("projects-none-matching"));
      return;
    }
    try {
      state.workRun = await invoke("start_work_run", {
        prompt,
        projects: listed.map((item) => item.path),
      });
      showToast(t("work-started", { count: listed.length }), "success");
    } catch (error) {
      showToast(String(error));
    }
    renderWorkRun();
  }

  return {
    renderWorkRun,
    refreshWorkRun,
    setWorkRunPaused,
    workEntryAction,
    renderWorkSchedule,
    refreshWorkSchedule,
    setWorkSchedule,
    setWorkSchedulePaused,
    renderPromptLibrary,
    loadStoredPrompts,
    openPromptLibrary,
    newStoredPrompt,
    selectStoredPrompt,
    saveStoredPrompt,
    deleteStoredPrompt,
    listedProjects,
    startWorkRun,
  };
}
