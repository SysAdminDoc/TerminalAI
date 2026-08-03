/**
 * Reading a project's roadmap state for the projects table.
 *
 * Split out of `main.js` for the reason the other modules were: every function
 * here can turn "I don't know" into a confident number, and in a table whose
 * whole purpose is deciding where to spend the next session, that is the error
 * that actually costs something.
 *
 * The rule: **unknown is a third state, never zero.** A project with no
 * `ROADMAP.md`, and a project whose roadmap is prose rather than checkboxes,
 * both have an unknown amount of work queued. Rendering either as `0` sorts it
 * beside a project that is genuinely finished and quietly removes it from
 * consideration.
 */

/** Milliseconds in a day. */
const DAY = 24 * 60 * 60 * 1000;

/**
 * Unwrap the serde `SystemTime` wire shape.
 *
 * Serde writes `SystemTime` as `{secs_since_epoch, nanos_since_epoch}`, not as
 * a number — reading it as one yields `NaN`, which formats as a plausible-
 * looking "today".
 */
export function modifiedMillis(summary) {
  const modified = summary?.modified;
  if (!modified) return null;
  if (typeof modified === "number") return modified * 1000;
  const seconds = modified.secs_since_epoch;
  if (typeof seconds !== "number") return null;
  return seconds * 1000 + Math.floor((modified.nanos_since_epoch ?? 0) / 1e6);
}

/** How long ago the roadmap was touched, or null when there is no roadmap. */
export function staleness(summary, now = Date.now()) {
  const millis = modifiedMillis(summary);
  if (millis === null) return null;
  return Math.max(0, now - millis);
}

/** A short, honest description of how stale a roadmap is. */
export function stalenessLabel(summary, t, now = Date.now()) {
  const elapsed = staleness(summary, now);
  if (elapsed === null) return null;
  const days = Math.floor(elapsed / DAY);
  if (days < 1) return t("touched-today");
  if (days < 60) return t("touched-days", { days });
  return t("touched-months", { months: Math.floor(days / 30) });
}

/**
 * Open items as a cell value.
 *
 * Returns the number only when the parser actually counted; otherwise the
 * reason it could not, so the table says "no roadmap" rather than "0".
 */
export function openItemsCell(summary, t) {
  const state = summary?.state;
  if (state?.kind === "counted") return { known: true, text: String(state.open), open: state.open };
  if (state?.kind === "no_checklist") return { known: false, text: t("projects-unreadable"), open: null };
  return { known: false, text: t("projects-no-roadmap"), open: null };
}

export function hasOpenWork(summary) {
  return summary?.state?.kind === "counted" && summary.state.open > 0;
}

/**
 * Order the table: most work first, then most recently touched.
 *
 * Projects whose count is unknown sort *after* every counted one rather than as
 * zero — they are candidates the operator may still want, just not ones this
 * tool can rank.
 */
export function sortProjects(projects = [], now = Date.now()) {
  return [...projects].sort((left, right) => {
    const leftOpen = left.roadmap?.state?.kind === "counted" ? left.roadmap.state.open : null;
    const rightOpen = right.roadmap?.state?.kind === "counted" ? right.roadmap.state.open : null;
    if (leftOpen !== rightOpen) {
      if (leftOpen === null) return 1;
      if (rightOpen === null) return -1;
      return rightOpen - leftOpen;
    }
    const leftAge = staleness(left.roadmap, now);
    const rightAge = staleness(right.roadmap, now);
    if (leftAge !== rightAge) {
      if (leftAge === null) return 1;
      if (rightAge === null) return -1;
      return leftAge - rightAge;
    }
    return String(left.name).localeCompare(String(right.name));
  });
}

/**
 * What the header says about coverage.
 *
 * The unknown count is always stated. "12 of 300 have open items" reads as a
 * complete survey; it is not one if 200 of those 300 have no roadmap at all.
 */
export function summarize(projects = []) {
  let withWork = 0;
  let unknown = 0;
  for (const project of projects) {
    if (hasOpenWork(project.roadmap)) withWork += 1;
    if (project.roadmap?.state?.kind !== "counted") unknown += 1;
  }
  return { withWork, unknown, total: projects.length };
}
