import assert from "node:assert/strict";
import { mkdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const testRoot = path.dirname(fileURLToPath(import.meta.url));
const artifactRoot = process.env.TERMINALAI_E2E_ARTIFACTS ?? path.join(testRoot, "..", "artifacts", "wdio");
mkdirSync(artifactRoot, { recursive: true });

const now = Math.floor(Date.now() / 1000);
const systemTime = (age = 90) => ({ secs_since_epoch: now - age, nanos_since_epoch: 0 });
const statusHistory = [{
  at: systemTime(120),
  from: "starting",
  to: "working",
  source: "hook",
  detail: "Synthetic WebdriverIO fleet fixture",
}];

const session = {
  id: "s0001",
  agent: "claude",
  name: "Demo API",
  cwd: "C:\\Users\\test\\repos\\demo-api",
  branch: "feature/demo",
  ports: [42000, 42001, 42002, 42003],
  model: "sonnet",
  effort: "high",
  status: "working",
  phase: "working",
  health: "healthy",
  restarts: 0,
  last_exit_code: null,
  backoff_until: null,
  state_since: systemTime(120),
  pid: 4242,
  last_line: "Implementing the API boundary",
  tool_progress: { completed: 3, total: 7 },
  resume_id: "native-demo-session",
  started_at: systemTime(600),
  status_since: systemTime(120),
  cost_usd: 1.25,
  unread: false,
  pinned: false,
  status_history: statusHistory,
  reviewed: false,
};

const fleet = (sessions = [session], focused = "s0001") => ({
  sessions,
  focused,
  admission: {
    max_live_sessions: 3,
    live_sessions: sessions.length,
    queued_sessions: 0,
    aggregate_cost_usd: sessions.length ? 1.25 : 0,
    dropped_events: 0,
  },
  store_quarantine: null,
});

const readyPreflight = {
  checks: [
    { id: "claude", label: "Claude Code CLI", state: "ok", detected: "claude 1.0.0", detail: null, can_fix: false },
    { id: "codex", label: "Codex CLI", state: "ok", detected: "codex 1.0.0", detail: null, can_fix: false },
    { id: "hooks", label: "Managed agent hooks", state: "ok", detected: "Installed", detail: null, can_fix: false },
    { id: "daemon", label: "TerminalAI daemon", state: "ok", detected: "Reachable", detail: null, can_fix: false },
    { id: "shortcut", label: "Start Menu shortcut", state: "ok", detected: "Installed", detail: null, can_fix: false },
  ],
};

const unreachablePreflight = {
  checks: readyPreflight.checks.map((check) => check.id === "daemon"
    ? { ...check, state: "error", detected: "Unavailable", detail: "Synthetic daemon outage", can_fix: true }
    : check),
};

const review = {
  entries: [{
    session_id: "s0001",
    name: "Demo API",
    agent: "claude",
    cwd: session.cwd,
    files_changed: 2,
    additions: 18,
    deletions: 4,
    review_cost: 22,
    conflicts: ["src/api.ts"],
    conflict_markers: 2,
    reviewed: false,
    diff: "@@ -1,2 +1,4 @@\n+export const demo = true;",
    diff_truncated: false,
    timed_out: false,
    error: null,
  }],
};

async function mockCommand(name, value) {
  const mock = await browser.tauri.mock(name);
  await mock.mockReturnValue(value);
  return mock;
}

async function dispatchClick(selector) {
  await browser.tauri.execute((tauri, target) => {
    const element = document.querySelector(target);
    if (!element) throw new Error(`missing UI target: ${target}`);
    element.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  }, selector);
}

async function assertScreenshot(name) {
  const target = path.join(artifactRoot, name);
  await browser.saveScreenshot(target);
  const size = statSync(target).size;
  assert.ok(size > 1024, `${name} should contain a real rendered capture`);
  assert.equal(readFileSync(target).subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
}

describe("TerminalAI desktop surface", () => {
  it("renders fleet, launcher, review, diagnostics, loading, empty, error, and daemon-unreachable states", async () => {
    await browser.pause(3000);
    assert.equal(await browser.$("#fleet-loading").isDisplayed(), true, "startup must expose loading state before mocks release it");

    const fleetMock = await mockCommand("fleet_snapshot", fleet());
    await mockCommand("preflight_report", readyPreflight);
    await mockCommand("review_snapshot", review);
    await mockCommand("list_presets", []);
    await mockCommand("app_version", "0.1.0");
    await mockCommand("attach_session_output", null);
    await mockCommand("mark_read", null);
    await mockCommand("preview_launch", "claude --model sonnet");

    await browser.tauri.execute(() => window.dispatchEvent(new Event("terminalai-wdio-ready")));
    await browser.$('#fleet-list [role="option"]').waitForDisplayed();
    assert.match(await browser.$("#fleet-list").getText(), /Demo API/);
    await assertScreenshot("fleet.png");

    await dispatchClick("#new-session-button");
    await browser.$("#launcher-dialog[open]").waitForDisplayed();
    assert.match(await browser.$("#launcher-dialog").getText(), /Launch an agent/);
    await assertScreenshot("launcher.png");
    await dispatchClick("#cancel-launch-button");

    await dispatchClick("#review-toggle");
    await browser.$("#review-view .review-entry").waitForDisplayed();
    assert.match(await browser.$("#review-view").getText(), /Conflict markers surfaced/);
    await assertScreenshot("review.png");
    await dispatchClick("#review-toggle");

    await dispatchClick("#diagnostics-toggle");
    await browser.$("#diagnostics-host .diagnostics-heading").waitForDisplayed();
    assert.match(await browser.$("#diagnostics-host").getText(), /WHY THIS STATE/);
    await assertScreenshot("diagnostics.png");
    await dispatchClick("#diagnostics-toggle");

    await fleetMock.mockReturnValue(fleet([]));
    await dispatchClick("#refresh-button");
    await browser.$("#empty-state").waitForDisplayed();
    assert.match(await browser.$("#empty-state").getText(), /No sessions yet/);

    await fleetMock.mockRejectedValue("Synthetic daemon unavailable");
    const preflightMock = await browser.tauri.mock("preflight_report");
    await preflightMock.mockReturnValue(unreachablePreflight);
    await dispatchClick("#refresh-button");
    await browser.$("#preflight-view").waitForDisplayed();
    assert.match(await browser.$("#preflight-list").getText(), /Synthetic daemon outage/);
    assert.equal(await browser.$("#preflight-list .tone-red").isDisplayed(), true);
    await assertScreenshot("daemon-unreachable.png");
  });
});
