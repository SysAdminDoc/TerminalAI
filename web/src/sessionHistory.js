import { optionalSystemTimeMs } from "./time.js";

/**
 * Render the archive of finished sessions.
 *
 * The store has carried these records since the first release and read them
 * back only to advance the id counter. They hold the layout, the folder and the
 * exact command — never output, which is deliberately dropped with the row — so
 * this view can say what ran and offer to run it again, and nothing more.
 *
 * `escape` and `translate` are passed in rather than imported so this module
 * stays testable without a DOM or a loaded catalog, matching the other
 * extracted renderers.
 */
export function renderSessionHistory(archives, { escape, translate, formatTime }) {
  if (!archives.length) {
    return `<p class="rollup-total">${escape(translate("session-history-empty"))}</p>`;
  }
  const rows = archives
    .map((archive) => {
      const when = archivedLabel(archive, formatTime);
      const cells = [
        `<td><span class="agent-badge agent-${escape(archive.agent)}">${escape(
          archive.agent === "codex" ? "CX" : "CC",
        )}</span> ${escape(archive.name)}</td>`,
        `<td class="history-folder" title="${escape(archive.cwd)}">${escape(folderLabel(archive.cwd))}</td>`,
        `<td class="history-command"><code>${escape(archive.command)}</code></td>`,
        `<td>${escape(when)}</td>`,
        `<td><button type="button" class="button button-quiet" data-relaunch="${escape(
          archive.id,
        )}">${escape(translate("session-history-relaunch"))}</button></td>`,
      ];
      return `<tr>${cells.join("")}</tr>`;
    })
    .join("");
  const headers = [
    "session-history-column-session",
    "session-history-column-folder",
    "session-history-column-command",
    "session-history-column-archived",
  ]
    .map((key) => `<th>${escape(translate(key))}</th>`)
    .join("");
  return `<table class="rollup-table">
      <thead><tr>${headers}<th></th></tr></thead>
      <tbody>${rows}</tbody>
    </table>
    <p class="settings-note">${escape(translate("session-history-relaunch-note"))}</p>`;
}

/**
 * A record written before archives carried a timestamp shows an em dash rather
 * than a fabricated date. Reading an absent stamp as the epoch would date every
 * pre-upgrade session to 1970 and look like data, not like a gap.
 */
export function archivedLabel(archive, formatTime) {
  const at = optionalSystemTimeMs(archive.archived_at);
  return at === null ? "—" : formatTime(at);
}

/** The last two path segments, which is what distinguishes sibling checkouts. */
export function folderLabel(cwd) {
  const parts = String(cwd ?? "")
    .split(/[\\/]+/)
    .filter(Boolean);
  return parts.slice(-2).join("/") || String(cwd ?? "");
}
