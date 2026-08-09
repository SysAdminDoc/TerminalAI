/** Persistent fleet-level banners that are not owned by a feature dialog. */
export function createFleetNotices({ $, state, t }) {
  /// The fleet's state is not reaching disk. This is deliberately not
  /// dismissable: the condition clears itself when a write succeeds.
  function renderStoreWriteError() {
    const banner = $("store-write-banner");
    const error = state.storeWriteError;
    banner.classList.toggle("view-hidden", !error);
    $("store-write-message").textContent = error
      ? t("store-write-failed-detail", { error })
      : "";
  }

  function renderStoreQuarantine() {
    const banner = $("store-quarantine-banner");
    const path = state.storeQuarantine;
    const visible = Boolean(path) && !state.storeQuarantineDismissed;
    banner.classList.toggle("view-hidden", !visible);
    $("store-quarantine-message").textContent = path
      ? t("store-quarantined-detail", { path })
      : "";
  }

  /// One banner for the whole fleet, not one failure per queued entry. A probe
  /// that could not run reports unknown and is deliberately absent here.
  function renderAuthBanner() {
    const banner = $("auth-banner");
    const expired = Array.isArray(state.admission.expired_auth) ? state.admission.expired_auth : [];
    banner.classList.toggle("view-hidden", expired.length === 0);
    if (!expired.length) return;
    const agents = expired
      .map((entry) => (entry.agent === "codex" ? "Codex" : "Claude Code"))
      .join(", ");
    $("auth-banner-message").textContent = t("auth-expired-detail", { agents });
  }

  return { renderAuthBanner, renderStoreQuarantine, renderStoreWriteError };
}
