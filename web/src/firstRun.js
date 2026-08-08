/**
 * Deterministic first-run guidance data.
 *
 * The demo is deliberately a frontend-only fixture. It never names a real
 * executable, does not call the daemon, and gives the first-run path something
 * an operator can explore before trusting a real agent with a repository.
 */

export const FIRST_RUN_STEP_IDS = Object.freeze(["project", "demo", "launcher"]);
export const FIRST_RUN_DEMO_PREFIX = "first-run-demo-";

const DEMO_STATUSES = Object.freeze([
  "needs-approval",
  "awaiting-input",
  "needs-you",
  "rate-limited",
  "working",
  "thinking",
  "idle",
  "starting",
  "queued",
  "unknown",
  "exited",
]);

const DEMO_LINES = Object.freeze({
  "needs-approval": "Waiting for permission before writing the migration",
  "awaiting-input": "Which API version should the demo use?",
  "needs-you": "The sample run stopped with a decision for you",
  "rate-limited": "Provider limit reached; waiting for the reset window",
  working: "Running the sample test suite",
  thinking: "Planning the next safe sample step",
  idle: "Finished the sample turn; ready for a prompt",
  starting: "Starting the sample session",
  queued: "Waiting for a free demo slot",
  unknown: "No state has been reported by this sample yet",
  exited: "The sample session has ended",
});

function systemTime(seconds) {
  return { secs_since_epoch: seconds, nanos_since_epoch: 0 };
}

function demoPhase(status) {
  if (status === "starting") return "preparing";
  if (status === "queued") return "queued";
  if (status === "exited") return "finished";
  return "working";
}

/** Return one safe sample row for every status the fleet can explain. */
export function createFirstRunDemoSessions(now = Date.now()) {
  const nowSeconds = Math.floor(Number(now) / 1000);
  return DEMO_STATUSES.map((status, index) => {
    const age = 45 + index * 23;
    const attention = ["needs-approval", "awaiting-input", "needs-you"].includes(status);
    return {
      id: `${FIRST_RUN_DEMO_PREFIX}${status}`,
      agent: index % 2 === 0 ? "claude" : "codex",
      name: `Demo · ${status}`,
      cwd: "C:\\Users\\you\\repos\\terminalai-demo",
      branch: `demo/${status}`,
      ports: [42000 + index],
      model: index % 2 === 0 ? "sonnet" : "gpt-5-codex",
      effort: "medium",
      status,
      phase: demoPhase(status),
      health: "healthy",
      restarts: 0,
      last_exit_code: status === "exited" ? 0 : null,
      backoff_until: null,
      state_since: systemTime(nowSeconds - age),
      pid: 9000 + index,
      last_line: DEMO_LINES[status],
      tool_progress: { completed: Math.min(index + 1, 4), total: 4 },
      resume_id: status === "exited" ? `${FIRST_RUN_DEMO_PREFIX}resume` : null,
      started_at: systemTime(nowSeconds - age - 240),
      status_since: systemTime(nowSeconds - age),
      cost_usd: 0.12 + index * 0.04,
      unread: attention,
      pinned: false,
      status_history: [],
      reviewed: false,
      memory_bytes: 32 * 1024 * 1024,
      memory_processes: 1,
    };
  });
}

export function isFirstRunDemoSession(sessionOrId) {
  const id = typeof sessionOrId === "string" ? sessionOrId : sessionOrId?.id;
  return typeof id === "string" && id.startsWith(FIRST_RUN_DEMO_PREFIX);
}

export function demoStatusCount(sessions) {
  return new Set(
    (Array.isArray(sessions) ? sessions : [])
      .filter(isFirstRunDemoSession)
      .map((session) => session.status),
  ).size;
}

export function emptyFirstRunProgress() {
  return Object.fromEntries(FIRST_RUN_STEP_IDS.map((id) => [id, false]));
}

export function normalizeFirstRunProgress(value) {
  const progress = emptyFirstRunProgress();
  if (!value || typeof value !== "object") return progress;
  for (const id of FIRST_RUN_STEP_IDS) progress[id] = value[id] === true;
  return progress;
}

export function readFirstRunProgress(storage) {
  try {
    const local = storage ?? (typeof globalThis !== "undefined" ? globalThis.localStorage : null);
    if (!local) return emptyFirstRunProgress();
    return normalizeFirstRunProgress(JSON.parse(local.getItem("terminalai.first-run.v1")));
  } catch {
    return emptyFirstRunProgress();
  }
}

export function saveFirstRunProgress(progress, storage) {
  const normalized = normalizeFirstRunProgress(progress);
  try {
    const local = storage ?? (typeof globalThis !== "undefined" ? globalThis.localStorage : null);
    local?.setItem("terminalai.first-run.v1", JSON.stringify(normalized));
  } catch {
    // A locked-down WebView can refuse storage. The in-memory checklist still
    // works, and first-run guidance must not make the shell fail to start.
  }
  return normalized;
}
