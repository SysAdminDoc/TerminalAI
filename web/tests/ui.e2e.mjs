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

// `focused` defaults to the session in the fixture and to nothing when there is
// none: a fleet with no sessions cannot have a focused one, and asking the
// window to reattach a pane for a session the snapshot does not contain is a
// state the daemon never produces.
const fleet = (sessions = [session], focused = sessions[0]?.id ?? null) => ({
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

/// The native packaged build does not expose `window.__TAURI__.event` because
/// the application imports the event API from its bundle. Invoke the same
/// plugin command that `@tauri-apps/api/event` calls so this test exercises the
/// real event transport rather than relying on the convenience global.
async function emitNativeEvent(name, payload) {
  await browser.tauri.execute((tauri, event, value) => tauri.core.invoke("plugin:event|emit", {
    event,
    payload: value,
  }), name, payload);
}

/// A capture of the real shipping window, kept after the run.
///
/// The screenshots are the point of driving WebView2 rather than jsdom, so the
/// runner no longer deletes them on the way out — a green run with nothing to
/// look at still leaves "does the shipping shell render the fleet" on trust.
async function assertScreenshot(name) {
  const target = path.join(artifactRoot, name);
  await browser.saveScreenshot(target);
  const size = statSync(target).size;
  assert.ok(size > 1024, `${name} should contain a real rendered capture`);
  assert.equal(readFileSync(target).subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
}

describe("TerminalAI desktop surface", () => {
  it("renders fleet, launcher, review, diagnostics, loading, empty, error, and daemon-unreachable states", async () => {
    // Waited for rather than sampled at a fixed instant: WebView2's first paint
    // on a cold private desktop takes longer than any pause worth hard-coding,
    // and the window deliberately holds this state until the mocks are in
    // place, so there is nothing to race with once it appears.
    await browser.$("#fleet-loading").waitForDisplayed({
      timeout: 30000,
      timeoutMsg: "startup must expose loading state before mocks release it",
    });

    const fleetMock = await mockCommand("fleet_snapshot", fleet());
    await mockCommand("preflight_report", readyPreflight);
    await mockCommand("review_snapshot", review);
    await mockCommand("list_presets", []);
    await mockCommand("app_version", "0.1.0");
    const attachOutputMock = await mockCommand("attach_session_output", null);
    const resizeMock = await mockCommand("resize_session", null);
    await mockCommand("focus_session", null);
    await mockCommand("subscribe_output", null);
    await mockCommand("stream_scrollback", null);
    await mockCommand("mark_read", null);
    await mockCommand("preview_launch", "claude --model sonnet");

    await browser.tauri.execute(() => window.dispatchEvent(new Event("terminalai-wdio-ready")));
    await browser.$('#fleet-list [role="option"]').waitForDisplayed();
    assert.match(await browser.$("#fleet-list").getText(), /Demo API/);
    await browser.waitUntil(() => attachOutputMock.calls.length > 0, {
      timeout: 30000,
      timeoutMsg: "the focused session must attach its output channel in the packaged shell",
    });
    await browser.waitUntil(() => resizeMock.calls.length > 0, {
      timeout: 30000,
      timeoutMsg: "the packaged shell must send the measured terminal geometry",
    });
    await assertScreenshot("fleet.png");

    await emitNativeEvent("terminalai:event", {
      kind: "session-updated",
      session: {
        ...session,
        status: "awaiting-input",
        phase: "awaiting-input",
        last_line: "Waiting for operator input",
        status_since: systemTime(0),
        state_since: systemTime(0),
      },
    });
    const updatedRow = browser.$('#fleet-list [data-id="s0001"]');
    await updatedRow.waitForDisplayed();
    await browser.waitUntil(async () => (await updatedRow.getAttribute("aria-label"))?.includes("Awaiting input"), {
      timeout: 10000,
      timeoutMsg: "a real Tauri event must update the accessible session row",
    });

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

    // The repeating work run, driven in the real WebView: a schedule is the one
    // feature the operator is not present for, so the window has to say what it
    // is about to do without being asked.
    await mockCommand("scan_projects", [{
      name: "demo-api",
      path: session.cwd,
      modified: systemTime(600),
      roadmap: { open_items: 3, checked_items: 1, modified: systemTime(600), path: `${session.cwd}\ROADMAP.md` },
    }]);
    await mockCommand("list_project_roots", ["C:\Users\test\repos"]);
    await mockCommand("list_stored_prompts", [{ name: "Drain the roadmap", text: "Work the roadmap.", source: null }]);
    await mockCommand("work_run", null);
    await mockCommand("work_schedule", null);
    const scheduleMock = await mockCommand("set_work_schedule", {
      prompt: "Drain the roadmap",
      projects: [session.cwd],
      interval_seconds: 14400,
      next_due: systemTime(-4 * 3600),
      paused: false,
      history: [{ at: systemTime(60), result: { kind: "skipped", reason: "the previous run was still going" }, missed: 2 }],
    });
    await dispatchClick("#projects-toggle");
    await browser.$("#projects-dialog[open]").waitForDisplayed();
    await browser.tauri.execute(() => {
      const select = document.querySelector("#work-repeat-select");
      select.value = "14400";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await browser.$("#work-schedule-status").waitForDisplayed();
    const scheduleLine = await browser.$("#work-schedule-status").getText();
    assert.match(scheduleLine, /Next run in 4 hours/);
    assert.match(scheduleLine, /the previous run was still going/, "a firing that did nothing must still be reported");
    assert.match(scheduleLine, /2 occurrences were missed/);
    assert.equal(await browser.$("#work-schedule-pause-button").isDisplayed(), true);
    assert.equal(scheduleMock.calls.length, 1, "the cadence control did not reach the backend");
    await assertScreenshot("work-schedule.png");
    await dispatchClick("#close-projects-button");

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

    await fleetMock.mockReturnValue(fleet());
    await preflightMock.mockReturnValue(readyPreflight);
    // Recovery is driven through the operator-facing recheck control. It
    // refreshes both health and fleet state, then closes the outage surface
    // only after the daemon reports that it is ready again.
    await dispatchClick('#preflight-list button[data-preflight-action="recheck"][data-preflight-id="daemon"]');
    await browser.$('#fleet-list [data-id="s0001"]').waitForDisplayed();
    assert.equal(await browser.$("#preflight-view").isDisplayed(), false);
    assert.match(await browser.$('#fleet-list [data-id="s0001"]').getAttribute("aria-label"), /Demo API/);
    assert.ok(attachOutputMock.calls.length > 1, "reconnecting must reattach the focused session output");
    await assertScreenshot("daemon-recovered.png");
  });
});
