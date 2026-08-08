/**
 * Read-only rows for agent sessions started outside TerminalAI.
 *
 * The supervisor can observe these sessions but does not own their pty or
 * process handle. Keeping the renderer behind a small boundary makes that
 * non-actionable contract explicit.
 */
const EXTERNAL_STATE_LABEL = Object.freeze({
  live: { label: "external-running", tone: "sapphire" },
  ended: { label: "external-ended", tone: "overlay0" },
  unknown: { label: "external-unknown", tone: "overlay0" },
});

export function createExternalSessions(deps) {
  const {
    $,
    countMessage,
    escapeHtml,
    folderLabel,
    invoke,
    metaLabel,
    state,
    t,
  } = deps;

  /// What the agent said about itself, kept in its own words.
  ///
  /// `claude agents --json` returns `state` (working | blocked | done | failed |
  /// stopped) and, when blocked, `waitingFor`. The panel already ran that
  /// command and threw both away, so a row said "Running" while the agent had
  /// reported it was waiting on a permission prompt.
  function externalReportedLabel(session) {
    const reportedState = String(session?.reported_state ?? "").trim();
    if (!reportedState) return "";
    const waiting = String(session?.waiting_for ?? "").trim();
    return waiting
      ? t("external-blocked-on", { state: reportedState, waiting })
      : reportedState;
  }

  // Rows for sessions this supervisor did not start. Deliberately actionless:
  // we do not own their pty, so offering Stop or Focus would promise something
  // the daemon cannot deliver.
  function renderExternal() {
    const supervised = new Set(
      state.sessions.map((session) => String(session.cwd ?? "").toLowerCase()),
    );
    const rows = state.external.filter((session) => session.state !== "ended");
    const view = $("external-view");
    view.classList.toggle("view-hidden", rows.length === 0 && !state.externalError);

    if (state.externalError) {
      $("external-summary").textContent = state.externalError;
      $("external-list").innerHTML = "";
      return;
    }
    const unknown = rows.filter((session) => session.state === "unknown").length;
    $("external-summary").textContent = unknown
      ? `${countMessage("count-external", rows.length)} · ${countMessage("count-unknown-external", unknown)}`
      : countMessage("count-external", rows.length);

    $("external-list").innerHTML = rows
      .map((session) => {
        const meta = EXTERNAL_STATE_LABEL[session.state] ?? EXTERNAL_STATE_LABEL.unknown;
        const label = session.name || folderLabel(session.cwd) || `pid ${session.pid}`;
        const where = session.entrypoint
          ? `${session.kind ?? "session"} · ${session.entrypoint}`
          : session.kind ?? "session";
        const alsoHere = supervised.has(String(session.cwd ?? "").toLowerCase())
          ? '<span class="external-overlap" title="' +
            escapeHtml(t("external-same-folder")) +
            '">' +
            escapeHtml(t("external-same-folder-short")) +
            "</span>"
          : "";
        // The agent's own vocabulary, not ours. Process liveness says the thing
        // is running; only this says whether it is blocked on a permission
        // prompt, and collapsing the two made every live row read "Running".
        const reported = externalReportedLabel(session);
        const stateText = reported
          ? `${metaLabel(meta)} · ${reported}`
          : metaLabel(meta);
        const externalAriaLabel = `${label}, ${stateText}, ${countMessage("count-external", 1)}`;
        return [
          `<article class="external-row" role="listitem" aria-label="${escapeHtml(externalAriaLabel)}">\n`,
          `<span class="status-glyph tone-${escapeHtml(meta.tone)}" aria-hidden="true">◦</span>`,
          `<div class="external-identity"><div class="external-name">${escapeHtml(label)}</div>`,
          `<div class="external-meta"><span title="${escapeHtml(String(session.cwd ?? ""))}">`,
          `${escapeHtml(folderLabel(session.cwd))}</span><span>${escapeHtml(where)}</span>`,
          `${session.version ? `<span>v${escapeHtml(session.version)}</span>` : ""}</div></div>`,
          `<span class="external-state"${
            reported ? ` title="${escapeHtml(t("external-reported-by-agent"))}"` : ""
          }>${escapeHtml(stateText)}</span>`,
          `<span class="external-pid" title="${escapeHtml(t("external-process-id"))}">`,
          `${escapeHtml(String(session.pid))}</span>`,
          alsoHere,
          "\n      </article>",
        ].join("");
      })
      .join("");
  }

  async function loadExternal() {
    try {
      state.external = (await invoke("external_sessions")) ?? [];
      state.externalError = null;
    } catch (error) {
      // Never render "nothing running" from a failed lookup.
      state.external = [];
      state.externalError = t("external-load-error", { error: String(error) });
    }
    renderExternal();
  }

  return { renderExternal, loadExternal };
}
