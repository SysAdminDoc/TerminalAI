/**
 * Is there a newer TerminalAI than the one running?
 *
 * Split out of `main.js` for size. The version comparison is the part worth
 * having on its own: it decides whether the operator is told to go and get
 * something, and a wrong answer either nags about a release they already have
 * or stays quiet about one they do not.
 *
 * Nothing is ever installed automatically. The check reports; the operator
 * decides.
 */

export const RELEASES_ENDPOINT =
  "https://api.github.com/repos/SysAdminDoc/TerminalAI/releases/latest";

/// Where the operator goes when the check says there is something newer. The
/// endpoint above answers the question; this is the page that answers it.
export const RELEASES_PAGE = "https://github.com/SysAdminDoc/TerminalAI/releases/latest";

export function versionTuple(value) {
  const match = String(value ?? "").replace(/^v/i, "").match(/^(\d+)\.(\d+)\.(\d+)/);
  return match ? match.slice(1).map(Number) : null;
}

export function isNewerVersion(candidate, current) {
  const next = versionTuple(candidate);
  const installed = versionTuple(current);
  if (!next || !installed) return false;
  for (let index = 0; index < next.length; index += 1) {
    if (next[index] !== installed[index]) return next[index] > installed[index];
  }
  return false;
}

/**
 * Run the check and report the result.
 *
 * Takes its collaborators rather than reaching for module scope — the same
 * shape `createRowRenderer` uses. `$` reaches the button, `t` supplies the copy,
 * `invoke` asks the backend which version is installed, `state` caches that,
 * `showToast` carries the outcomes that need no action and `showUpdateResult`
 * carries the one that does.
 */
export async function checkForUpdates({
  $,
  t,
  invoke,
  state,
  showToast,
  showUpdateResult,
  fallbackVersion,
  fetch: fetchImpl = globalThis.fetch,
}) {
  const button = $("update-check-button");
  if (button.disabled) return;
  button.disabled = true;
  showUpdateResult(null);
  button.querySelector("span").textContent = t("update-checking");
  try {
    const current = state.appVersion ?? await invoke("app_version").catch(() => fallbackVersion);
    state.appVersion = current;
    const response = await fetchImpl(RELEASES_ENDPOINT, {
      headers: {
        Accept: "application/vnd.github+json",
      },
    });
    if (response.status === 404) {
      showToast(t("update-newest", { version: current }), "success");
      return;
    }
    if (!response.ok) throw new Error(t("update-http-error", { status: response.status }));
    const release = await response.json();
    const latest = String(release.tag_name ?? "").replace(/^v/i, "");
    if (!versionTuple(latest)) throw new Error(t("update-invalid-release"));
    if (isNewerVersion(latest, current)) {
      showUpdateResult(t("update-available", { latest, current }));
    } else {
      showToast(t("update-up-to-date", { version: current }), "success");
    }
  } catch (error) {
    showToast(t("update-failed", { error: String(error) }));
  } finally {
    button.disabled = false;
    button.querySelector("span").textContent = t("button-check-updates");
  }
}
