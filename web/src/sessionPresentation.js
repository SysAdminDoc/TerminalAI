import { formatCost } from "./rollup.js";
import { systemTimeMs } from "./time.js";

const AGENT_AUTO_RESOLVE_SECONDS = 60;

/** Pure row and dialog presentation semantics shared by the renderer shell. */
export function createSessionPresentation({ t, relativeDwell = (value) => value }) {
  function dwell(value) {
    return relativeDwell(value);
  }

  function toolProgress(value) {
    const completed = Number(value?.completed);
    const total = Number(value?.total);
    if (!Number.isInteger(completed) || !Number.isInteger(total) || completed < 0 || total <= 0) return "—";
    return `${Math.min(completed, total)}/${total}`;
  }

  // Number(null) is 0, so a session that has never reported a cost used to
  // render "$0.00" — a computed-looking zero is worse than an honest em dash.
  function cost(value) {
    return value === "" ? "—" : formatCost(value);
  }

  function costTitle(session) {
    const budget = Number(session?.budget_usd);
    if (!Number.isFinite(budget) || budget < 0) return t("cost-explained");
    const key = session?.budget_exhausted ? "cost-budget-spent" : "cost-budget-of";
    return t(key, { budget: formatCost(budget) });
  }

  function memoryTitle(session) {
    if (session?.memory_limited) return t("memory-limited-explained");
    const processes = Number(session?.memory_processes);
    if (!Number.isInteger(processes) || processes < 1) return t("memory-unscoped-explained");
    return t("memory-explained", { processes });
  }

  function ports(value) {
    const assigned = Array.isArray(value)
      ? value.map(Number).filter((port) => Number.isInteger(port) && port > 0 && port <= 65535)
      : [];
    if (!assigned.length) return "—";
    if (assigned.length > 1 && assigned.every((port, index) => index === 0 || port === assigned[index - 1] + 1)) {
      return String(assigned[0]) + "–" + String(assigned.at(-1));
    }
    return assigned.join(", ");
  }

  function folderLabel(path) {
    const parts = String(path ?? "").split(/[\\/]/).filter(Boolean);
    return parts.at(-1) ?? path ?? "—";
  }

  function memory(bytes) {
    const value = Number(bytes);
    if (!Number.isFinite(value) || value <= 0) return "—";
    if (value >= 1024 * 1024 * 1024) return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
    return `${Math.round(value / (1024 * 1024))} MB`;
  }

  function isAttention(session) {
    return ["needs-approval", "awaiting-input", "needs-you"].includes(session.status);
  }

  function expiresWithoutAnAnswer(session) {
    return ["awaiting-input", "needs-you"].includes(session?.status);
  }

  function answerSecondsRemaining(session, now = Date.now()) {
    if (!expiresWithoutAnAnswer(session)) return null;
    const since = systemTimeMs(session?.status_since);
    if (!Number.isFinite(since)) return null;
    const elapsed = (now - since) / 1000;
    if (elapsed < 0) return AGENT_AUTO_RESOLVE_SECONDS;
    return Math.max(0, Math.round(AGENT_AUTO_RESOLVE_SECONDS - elapsed));
  }

  function answerCountdownLabel(session, now = Date.now()) {
    const remaining = answerSecondsRemaining(session, now);
    if (remaining === null) return "";
    return remaining > 0
      ? t("answer-deadline", { seconds: remaining })
      : t("answer-deadline-passed");
  }

  return {
    answerCountdownLabel,
    answerSecondsRemaining,
    cost,
    costTitle,
    dwell,
    expiresWithoutAnAnswer,
    folderLabel,
    isAttention,
    memory,
    memoryTitle,
    ports,
    toolProgress,
  };
}
