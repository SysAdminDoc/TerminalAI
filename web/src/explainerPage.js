import { STATUS_KEYS, STATUS_META } from "./sessionStatus.js";

/**
 * Row-model explainer page orchestration.
 *
 * Status metadata remains shared with the fleet row renderer, so this page
 * cannot drift into a second list of meanings.
 */
export function createExplainerPage(deps) {
  const { $, escapeHtml, openWorkspacePage, renderGuarded, t } = deps;

  function renderExplainerStates() {
    $("explainer-states").innerHTML = STATUS_KEYS.map((status) => {
      const meta = STATUS_META[status];
      return (
        '<div class="explainer-state"><dt><span class="state-chip tone-' +
        escapeHtml(meta.tone) +
        '"><span class="state-chip-glyph" aria-hidden="true">' +
        meta.glyph +
        "</span><span>" +
        escapeHtml(t(meta.short)) +
        "</span></span></dt><dd>" +
        escapeHtml(t(meta.short + "-explained")) +
        "</dd></div>"
      );
    }).join("");
  }

  function openExplainer() {
    const dialog = $("explainer-dialog");
    openWorkspacePage(dialog);
    renderGuarded(
      $("explainer-states"),
      t("explainer-render-error"),
      "openExplainer",
      openExplainer,
      renderExplainerStates,
    );
  }

  return { openExplainer, renderExplainerStates };
}
