import { saveFirstRunProgress } from "./firstRun.js";

/** Render and persist the shell's first-run checklist. */
export function createFirstRunGuide({ $, state, t, saveProgress = saveFirstRunProgress }) {
  function renderFirstRunGuide() {
    const checklist = $("first-run-checklist");
    if (!checklist) return;
    const entries = Array.from(checklist.querySelectorAll("[data-first-run-step]"));
    const done = entries.filter((entry) => state.firstRunProgress[entry.dataset.firstRunStep]).length;
    $("first-run-progress").textContent = t("first-run-progress", {
      done,
      total: entries.length,
    });
    for (const entry of entries) {
      const complete = state.firstRunProgress[entry.dataset.firstRunStep] === true;
      entry.dataset.complete = String(complete);
      entry.querySelector(".first-run-step-state").textContent = t(
        complete ? "first-run-step-done" : "first-run-step-next",
      );
    }
  }

  function markFirstRunStep(step) {
    if (state.firstRunProgress[step] === true) return;
    state.firstRunProgress = saveProgress({
      ...state.firstRunProgress,
      [step]: true,
    });
    renderFirstRunGuide();
  }

  return { markFirstRunStep, renderFirstRunGuide };
}
