/** Keep the fleet and review surfaces mutually exclusive in the shell. */
export function createReviewVisibility({ $, state, t }) {
  function syncReviewVisibility() {
    const hidden = state.reviewMode || state.preflightMode;
    ["fleet-state-strip", "column-labels", "fleet-list", "empty-state"].forEach((id) => {
      $(id).classList.toggle("view-hidden", hidden);
    });
    $("review-view").classList.toggle("view-hidden", !state.reviewMode || state.preflightMode);
    $("review-toggle").setAttribute("aria-pressed", String(hidden));
    $("review-toggle").classList.toggle("wide-toggle-active", state.reviewMode && !state.preflightMode);
    $("review-toggle").textContent = state.reviewMode && !state.preflightMode
      ? t("button-fleet")
      : t("button-review");
  }

  return { syncReviewVisibility };
}
