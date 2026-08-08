/**
 * Focused terminal history and search controls.
 *
 * The xterm pane is already a module. This companion owns the bounded history
 * replay and find-bar protocol so the shell only coordinates the current focus.
 */
export function createTerminalHistory(deps) {
  const { $, state, invoke, t, showToast, Channel, document, writeTerminalBytes } = deps;

function createOutputChannel(id) {
  const generation = state.focusGeneration;
  const channel = new Channel();
  channel.onmessage = (data) => writeTerminalBytes(data, id, generation);
  return channel;
}

/// The focused pane already contains this in-memory ring after attach. Ask for
/// the ring plus one older window so the reset-and-replay path includes bytes
/// the pane did not already show.
const MAX_SCROLLBACK_BYTES = 512 * 1024;
const HISTORY_OLDER_BYTES = 128 * 1024;
const HISTORY_REQUEST_BYTES = MAX_SCROLLBACK_BYTES + HISTORY_OLDER_BYTES;

/// Show or hide the find bar over the focused pane.
///
/// Closing clears the search rather than leaving it: xterm keeps its highlight
/// decorations until told otherwise, so a hidden bar with a live search leaves
/// the pane marked up for a query the operator can no longer see.
function toggleFind(next = null) {
  const bar = $("terminal-find");
  const open = next === null ? bar.hidden : next;
  bar.hidden = !open;
  $("terminal-find-toggle").setAttribute("aria-pressed", String(open));
  if (open) {
    $("terminal-find-input").focus();
    $("terminal-find-input").select();
    runFind();
  } else {
    state.searchAddon?.clearDecorations();
    $("terminal-find-count").textContent = "";
    state.terminal?.focus();
  }
}

/// Run the current query. `direction` moves to the adjacent match; omitting it
/// re-runs in place, which is what a keystroke in the field wants.
function runFind(direction = null) {
  const needle = $("terminal-find-input").value;
  if (!state.searchAddon) return;
  if (!needle) {
    state.searchAddon.clearDecorations();
    $("terminal-find-count").textContent = "";
    return;
  }
  // Read from the same tokens `terminalTheme` uses, not written as literals:
  // decorations are painted into the same canvas no contrast gate can see, so
  // a hardcoded palette here would be the theming defect again in a place the
  // guard for it would not have looked.
  const styles = getComputedStyle(document.documentElement);
  const token = (name) => styles.getPropertyValue(name).trim();
  const options = {
    decorations: {
      matchOverviewRuler: token("--yellow"),
      activeMatchColorOverviewRuler: token("--red"),
      matchBackground: token("--term-selection"),
      activeMatchBackground: token("--yellow"),
    },
  };
  if (direction === "previous") state.searchAddon.findPrevious(needle, options);
  else state.searchAddon.findNext(needle, options);
}

/// Report the addon's own match count.
///
/// `resultCount` is -1 while the addon is still scanning a long buffer, and 0
/// when nothing matched. The two are different answers and the row says which:
/// showing "0 matches" during a scan is a wrong answer that arrives before the
/// right one.
function renderFindCount(results) {
  const element = $("terminal-find-count");
  if (!results || results.resultCount < 0) {
    element.textContent = t("find-searching");
    return;
  }
  if (results.resultCount === 0) {
    element.textContent = t("find-none");
    return;
  }
  element.textContent = t("find-position", {
    index: results.resultIndex + 1,
    total: results.resultCount,
  });
}

/// Prepend output the in-memory ring has already dropped.
///
/// The terminal is reset and rewritten rather than scrolled backwards: xterm has
/// no way to insert above existing content, and replaying history followed by
/// the ring is the only ordering that reads correctly. The live stream keeps
/// arriving on its own channel throughout.
async function loadOlderOutput() {
  const id = state.focused;
  if (!id || state.historyLoading) return;
  state.historyLoading = true;
  try {
    const generation = state.focusGeneration;
    const chunks = [];
    const channel = new Channel();
    channel.onmessage = (data) => chunks.push(data);
    await invoke("stream_scrollback_history", {
      id,
      maxBytes: HISTORY_REQUEST_BYTES,
      channel,
    });
    // A focus switch while the read was in flight would otherwise paint one
    // session's history into another session's pane.
    if (state.focused !== id || state.focusGeneration !== generation) return;
    const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
    if (!total) {
      showToast(t("history-empty"));
      return;
    }
    state.terminal?.reset();
    for (const chunk of chunks) writeTerminalBytes(chunk, id, generation);
    showToast(t("history-loaded", { bytes: Math.round(total / 1024) }), "success");
  } catch (error) {
    showToast(t("history-load-error", { error: String(error) }));
  } finally {
    state.historyLoading = false;
  }
}

async function attachSessionOutput(id) {
  const channel = createOutputChannel(id);
  state.outputChannel = channel;
  const session = state.sessions.find((item) => item.id === id);
  if (session && session.status !== "exited") {
    await invoke("attach_session_output", { id, channel });
  } else {
    await invoke("focus_session", { id });
    await invoke("subscribe_output", { id, channel });
    await invoke("stream_scrollback", { id, channel });
  }
  await invoke("mark_read", { id });
}

async function rowAction(action, id, row = null) {
  try {
    if (action === "queue") await openQueue(id);
    if (action === "pin") await invoke("toggle_pin", { id });
    if (action === "focus") await focusSession(id);
    if (action === "reply") {
      const input = row?.querySelector("input[data-reply]");
      const reply = input?.value.trim();
      if (!reply) return;
      const bracketedPaste = `\x1b[200~${reply}\x1b[201~\r`;
      await invoke("write_session", { id, data: bracketedPaste });
      await invoke("mark_read", { id });
      input.value = "";
      showToast(t("reply-sent"), "success");
    }
    if (action === "kill") {
      await invoke("kill_session", { id });
      showToast(t("stop-signal-sent"), "success");
    }
    if (action === "revive") {
      await invoke("revive_session", { id });
      showToast(t("resume-started"), "success");
    }
    if (action === "archive") {
      await invoke("archive_session", { id });
      showToast(t("archive-stopped"), "success");
    }
  } catch (error) {
    showToast(String(error));
  }
}

// Launcher behavior lives in `launcher.js`; this entry only binds the panel.

  return {
    createOutputChannel,
    toggleFind,
    runFind,
    renderFindCount,
    loadOlderOutput,
    attachSessionOutput,
    rowAction,
  };
}
