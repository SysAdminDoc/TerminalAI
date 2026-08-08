import { reconcileKeyedRows } from "./fleetRows.js";

/**
 * Low-frequency grid previews for pinned sessions.
 *
 * The focused terminal remains the only xterm instance. Pinned panes are
 * daemon-provided grid snapshots, reconciled by session id and polled slowly
 * enough to remain an orientation aid instead of a second terminal stream.
 */
export function createPinnedPanes(deps) {
  const { $, document, invoke, lifecycleLabel, state, STATUS_META, t } = deps;

  /// How often pinned panes re-read their grid.
  const PINNED_POLL_MS = 1000;
  /// The daemon refuses a fourth pin; the UI states the same number.
  const MAX_PINNED = 3;

  function pinnedSessions() {
    return state.sessions.filter((session) => session.pinned).slice(0, MAX_PINNED);
  }

  /// Render the split view beneath the focused terminal.
  ///
  /// Panes are keyed by session id and reconciled rather than rebuilt, so a
  /// snapshot arriving for one pane cannot scroll or blank another.
  function renderPinnedSplit() {
    const host = $("pinned-split");
    const pinned = pinnedSessions();
    host.hidden = pinned.length === 0;
    host.classList.toggle("pinned-split-active", pinned.length > 0);
    if (!pinned.length) {
      host.replaceChildren();
      state.pinnedGrids.clear();
      return;
    }
    reconcileKeyedRows(
      host,
      pinned,
      (session) => session.id,
      (session) => {
        const pane = document.createElement("article");
        pane.className = "pinned-pane";
        pane.dataset.id = session.id;
        pane.innerHTML =
          '<header class="pinned-pane-head"><span class="pinned-pane-name"></span>' +
          '<span class="pinned-pane-status"></span></header><pre class="pinned-pane-grid"></pre>';
        return pane;
      },
      (pane, session) => {
        pane.querySelector(".pinned-pane-name").textContent = session.name;
        const status = pane.querySelector(".pinned-pane-status");
        status.textContent = lifecycleLabel(session);
        status.className = `pinned-pane-status tone-${(STATUS_META[session.status] ?? STATUS_META.exited).tone}`;
        const grid = pane.querySelector(".pinned-pane-grid");
        const snapshot = state.pinnedGrids.get(session.id);
        // Until the first snapshot lands the pane says so rather than showing an
        // empty box that reads as "this session printed nothing".
        grid.textContent = snapshot ?? t("pinned-waiting");
        grid.classList.toggle("pinned-pane-grid-waiting", !snapshot);
      },
      () => false,
    );
  }

  /// Read each pinned session's grid and redraw the split view.
  async function refreshPinnedGrids() {
    const pinned = pinnedSessions();
    if (!pinned.length) return;
    const results = await Promise.all(
      pinned.map(async (session) => {
        try {
          const grid = await invoke("grid_snapshot", { id: session.id });
          // Trailing blank rows are most of an idle grid; dropping them keeps a
          // pane the height of its content instead of a fixed 40 lines.
          const lines = Array.isArray(grid?.lines) ? [...grid.lines] : [];
          while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
          return [session.id, lines.join("\n")];
        } catch {
          // A session that exited between the poll and the read is not an error
          // worth a toast; the row already says so.
          return [session.id, null];
        }
      }),
    );
    let changed = false;
    for (const [id, text] of results) {
      if (text === null) continue;
      if (state.pinnedGrids.get(id) !== text) {
        state.pinnedGrids.set(id, text);
        changed = true;
      }
    }
    // Drop grids for sessions that are no longer pinned, so unpinning and
    // repinning does not show a stale frame from minutes ago.
    const live = new Set(pinned.map((session) => session.id));
    for (const id of [...state.pinnedGrids.keys()]) {
      if (!live.has(id)) {
        state.pinnedGrids.delete(id);
        changed = true;
      }
    }
    if (changed) renderPinnedSplit();
  }

  function startPinnedPolling() {
    if (state.pinnedTimer) return;
    state.pinnedTimer = setInterval(() => void refreshPinnedGrids(), PINNED_POLL_MS);
  }

  return { pinnedSessions, refreshPinnedGrids, renderPinnedSplit, startPinnedPolling };
}
