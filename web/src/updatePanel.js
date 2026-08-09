import { checkForUpdates as runUpdateCheck } from "./updateCheck.js";

/** Bind the update check to the shell's button, result surface, and toast. */
export function createUpdatePanel({ $, fallbackVersion, invoke, showToast, state, t }) {
  /// A newer version is the one outcome that needs to remain actionable.
  function showUpdateResult(message) {
    const result = $("update-result");
    result.classList.toggle("view-hidden", !message);
    $("update-result-message").textContent = message ?? "";
  }

  function checkForUpdates() {
    return runUpdateCheck({
      $,
      t,
      invoke,
      state,
      showToast,
      showUpdateResult,
      fallbackVersion,
    });
  }

  return { checkForUpdates, showUpdateResult };
}
