/**
 * Review and landing page orchestration.
 *
 * The review surface owns its render and action contract; the shell only
 * supplies state, localization, and the daemon boundary.
 */
export function createReviewPage(deps) {
  const {
    $,
    countMessage,
    escapeHtml,
    folderLabel,
    invoke,
    renderDataError,
    showToast,
    state,
    t,
  } = deps;

  function reviewNumber(value) {

    const number = Number(value);

    return Number.isFinite(number) && number >= 0 ? Math.floor(number) : 0;

  }



  function renderReview() {
    if (state.reviewError) {
      $("review-summary").textContent = t("review-unavailable");
      $("review-empty").classList.add("view-hidden");
      renderDataError(
        $("review-list"),
        t("review-load-error", { error: state.reviewError }),
        "review",
        loadReview,
      );
      return;
    }
    const entries = Array.isArray(state.reviews) ? state.reviews : [];
    const pending = entries.filter((entry) => !entry.reviewed).length;
    const conflicts = entries.filter(
      (entry) =>
        (entry.conflicts?.length ?? 0) > 0 ||
        reviewNumber(entry.conflict_markers) > 0,
    ).length;
    const timedOut = entries.filter((entry) => entry.timed_out === true).length;
    const summary = [
      t("sessions-count", { count: entries.length }),
      countMessage("count-pending", pending),
      countMessage("count-conflict", conflicts),
    ];
    if (timedOut) summary.push(countMessage("count-timed-out", timedOut));
    $("review-summary").textContent = summary.join(" · ");
    $("review-empty").classList.toggle("view-hidden", entries.length > 0);
    $("review-list").innerHTML = entries.map(renderReviewEntry).join("");
  }

  function renderReviewEntry(entry) {
    const conflicts = Array.isArray(entry.conflicts) ? entry.conflicts : [];
    const markers = reviewNumber(entry.conflict_markers);
    const additions = reviewNumber(entry.additions);
    const deletions = reviewNumber(entry.deletions);
    const files = reviewNumber(entry.files_changed);
    const reviewCost = reviewNumber(entry.review_cost);
    const agent = entry.agent === "codex" ? "Codex" : "Claude Code";
    const status = entry.timed_out
      ? t("review-status-timed-out")
      : entry.reviewed
        ? t("review-status-reviewed")
        : t("review-status-pending");
    const conflictDetails = conflicts.length
      ? "<ul>" + conflicts.map((path) => "<li><code>" + escapeHtml(path) + "</code></li>").join("") + "</ul>"
      : "";
    const conflictMarkup = conflicts.length || markers
      ? '<div class="review-conflict" role="alert"><strong>' +
        escapeHtml(t("review-conflict-markers")) +
        "</strong><span>" +
        escapeHtml(countMessage("review-conflicted-file", conflicts.length)) +
        (markers ? " · " + escapeHtml(t("review-marker-lines", { count: markers })) : "") +
        "</span>" +
        conflictDetails +
        "</div>"
      : "";
    const reviewError = entry.error || entry.land_error;
    const errorMarkup = reviewError
      ? '<div class="review-error" role="alert">' +
        escapeHtml(reviewError) +
        "</div>"
      : "";
    const diffMarkup = entry.diff
      ? '<details class="review-diff" ' +
        (conflicts.length || markers ? "open" : "") +
        "><summary>" +
        escapeHtml(t("review-show-diff")) +
        (entry.diff_truncated ? " · " + escapeHtml(t("review-truncated")) : "") +
        "</summary><pre>" +
        escapeHtml(entry.diff) +
        "</pre></details>"
      : '<div class="review-no-diff">' +
        escapeHtml(t("review-no-diff")) +
        "</div>";
    const actionMarkup = entry.reviewed
      ? '<span class="reviewed-label">✓ ' + escapeHtml(t("review-reviewed")) + '</span>'
      : entry.error
        ? ""
      : '<button type="button" class="button button-secondary review-mark" data-review-action="mark-reviewed" ' +
        'data-review-id="' +
        escapeHtml(entry.session_id) +
        '">' +
        escapeHtml(t("review-mark-reviewed")) +
        '</button>';
    // Landing is offered only when this collection is trustworthy. A timed-out or
    // errored review describes a tree nobody read, and conflicts are refused by
    // the gate anyway — offering the button there would teach the operator that
    // the button sometimes does nothing.
    const canLand = !entry.error && !entry.timed_out && !conflicts.length && files > 0;
    const landMarkup = canLand
      ? '<button type="button" class="button button-secondary review-land" data-review-action="land" data-review-id="' +
        escapeHtml(entry.session_id) +
        '" data-review-cwd="' +
        escapeHtml(entry.cwd) +
        '" title="' +
        escapeHtml(t("review-land-hint")) +
        '">' +
        escapeHtml(t("review-land")) +
        "</button>"
      : "";
    return [
      '<article class="review-entry' +
        (entry.reviewed ? " review-entry-reviewed" : "") +
        (entry.timed_out ? " review-entry-timeout" : "") +
        '" role="listitem">',
      '<div class="review-entry-heading"><div><h3>' +
        escapeHtml(entry.name) +
        '</h3><div class="review-repo"><span>' +
        escapeHtml(folderLabel(entry.cwd)) +
        '</span><span>' +
        escapeHtml(agent) +
        '</span><code>' +
        escapeHtml(entry.session_id) +
        '</code></div></div><div class="review-entry-action">' +
        landMarkup +
        actionMarkup +
        "</div></div>",
      '<div class="review-metrics"><span>' +
        escapeHtml(countMessage("count-file", files)) +
        '</span><span class="review-additions">+' +
        additions +
        '</span><span class="review-deletions">−' +
        deletions +
        '</span><span>' +
        escapeHtml(t("review-cost", { cost: reviewCost })) +
        '</span><span class="review-state">' +
        escapeHtml(status) +
        "</span></div>",
      conflictMarkup,
      errorMarkup,
      diffMarkup,
      "</article>",
    ].join("");
  }


  async function loadReview() {
    try {
      const snapshot = await invoke("review_snapshot");
      state.reviews = snapshot.entries ?? [];
      state.reviewError = null;
    } catch (error) {
      state.reviews = [];
      state.reviewError = String(error);
    }
    renderReview();
  }


  async function landSession(id, cwd, button) {
    const entry = state.reviews.find((review) => review.session_id === id);
    if (!entry) return;
    if (button) {
      button.disabled = true;
      button.textContent = t("review-landing");
    }
    try {
      // Read at land time, not captured earlier: the operator may tick it while
      // reading the diff.
      const archiveOnSuccess = Boolean($("review-archive-on-land")?.checked);
      const outcome = await invoke("land_session", {
        request: {
          source: cwd,
          target: cwd,
          // Named so a successful landing is recorded on the row it came from —
          // the one fact that separates a finished session from an abandoned one.
          session: id,
          archive_on_success: archiveOnSuccess,
          // Pinned to what this review described, so a target that moved while
          // the operator was reading is refused rather than silently landed on.
          expected_target_head: entry.target_head ?? null,
          verify: [],
        },
      });
      if (outcome.outcome === "landed") {
        // The work landed either way. A refused archive is reported rather than
        // swallowed, because it names something the operator can act on — the
        // session is still running, or its worktree holds unmerged commits.
        showToast(landedText(outcome), outcome.archive?.archive === "refused" ? "" : "success");
        await loadReview();
        return;
      }
      // A refusal names one specific condition. Keep the whole reason on the
      // review entry: a toast disappears while the operator is still reading.
      const reason = t("review-land-refused", { reason: refusalText(outcome) });
      entry.land_error = reason;
      renderReview();
    } catch (error) {
      entry.land_error = t("review-land-refused", { reason: String(error) });
      renderReview();
    } finally {
      if (button) {
        button.disabled = false;
        button.textContent = t("review-land");
      }
    }
  }

  /// Turn a structured refusal into one line the operator can act on.
  /// What a successful landing did, including what became of its session.
  function landedText(outcome) {
    const files = outcome.files_changed;
    if (outcome.archive?.archive === "archived") return t("review-landed-archived", { files });
    if (outcome.archive?.archive === "refused")
      return t("review-landed-not-archived", { files, reason: outcome.archive.detail });
    return t("review-landed", { files });
  }

  function refusalText(outcome) {
    switch (outcome.reason) {
      case "target-moved":
        return t("land-target-moved", { expected: outcome.expected, found: outcome.found });
      case "target-dirty":
        return t("land-target-dirty", { paths: (outcome.paths ?? []).join(", ") });
      case "conflict-markers":
        return t("land-conflict-markers", { paths: (outcome.paths ?? []).join(", ") });
      case "patch-did-not-apply":
        return t("land-patch-stale", { detail: outcome.detail });
      case "verify-failed":
        return t("land-verify-failed", { command: outcome.command, output: outcome.output });
      case "verify-failed-and-not-reversed":
        return t("land-verify-not-reversed", {
          command: outcome.command,
          error: outcome.reversal_error,
        });
      case "nothing-to-land":
        return t("land-nothing");
      case "unavailable":
        return t("land-unavailable", { detail: outcome.detail });
      default:
        // A refusal reason this build does not know about must still be shown —
        // swallowing it would render as "nothing happened".
        return String(outcome.reason ?? outcome);
    }
  }

  async function markReviewed(id, button) {
    if (button) button.disabled = true;
    try {
      await invoke("mark_reviewed", { id });
      const entry = state.reviews.find((review) => review.session_id === id);
      if (entry) entry.reviewed = true;
      renderReview();
      showToast(t("review-marked"), "success");
    } catch (error) {
      if (button) button.disabled = false;
      showToast(t("review-mark-error", { error: String(error) }));
    }
  }


  return { renderReview, loadReview, landSession, markReviewed };
}
