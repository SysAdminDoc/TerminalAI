/**
 * Render the checkouts this tool created that no live session owns.
 *
 * Teardown deliberately keeps a branch holding unmerged work, which is right —
 * but nothing ever revisited it, so worktrees and branches accumulated silently
 * and their registrations outlived the directories.
 *
 * Only a fully merged checkout gets a Remove button. A branch holding commits
 * is listed with the count and no control at all: this view cannot show the
 * operator what those commits contain, so offering to delete them here would be
 * asking for a decision on evidence it has not presented. Git is where that
 * decision belongs. `Unknown` is treated the same way — "we could not tell"
 * must never resolve to "delete it".
 */
export function renderWorktrees(worktrees, { escape, translate }) {
  if (!worktrees.length) {
    return `<p class="rollup-total surface-empty">${escape(translate("worktrees-empty"))}</p>`;
  }
  const headers = ["worktrees-column-branch", "worktrees-column-repo", "worktrees-column-state"]
    .map((key) => `<th>${escape(translate(key))}</th>`)
    .join("");
  const rows = worktrees
    .map((item, index) => {
      const note = item.missing_directory
        ? ` <small>${escape(translate("worktrees-missing-directory"))}</small>`
        : "";
      const action = isRemovable(item)
        ? `<button type="button" class="button button-quiet" data-reap="${escape(index)}">${escape(
            translate("worktrees-remove"),
          )}</button>`
        : "";
      return `<tr>
        <td><code>${escape(item.branch)}</code>${note}</td>
        <td title="${escape(item.repo)}">${escape(shortRepo(item.repo))}</td>
        <td>${escape(stateLabel(item.state, translate))}</td>
        <td>${action}</td>
      </tr>`;
    })
    .join("");
  return `<table class="rollup-table">
      <thead><tr>${headers}<th></th></tr></thead>
      <tbody>${rows}</tbody>
    </table>`;
}

/** Only a fully merged branch. Everything else is reported, never offered. */
export function isRemovable(item) {
  return item?.state?.kind === "merged";
}

export function stateLabel(state, translate) {
  if (state?.kind === "merged") return translate("worktrees-state-merged");
  if (state?.kind === "unmerged") {
    return translate("worktrees-state-unmerged", { commits: state.commits ?? 0 });
  }
  return translate("worktrees-state-unknown", { detail: state?.detail ?? "" });
}

function shortRepo(repo) {
  const parts = String(repo ?? "")
    .split(/[\\/]+/)
    .filter(Boolean);
  return parts.slice(-2).join("/") || String(repo ?? "");
}
