import assert from "node:assert/strict";
import test from "node:test";

import { createStartup } from "../src/startup.js";

function startupHarness({ wdioBuild = false } = {}) {
  const calls = [];
  const listeners = new Map();
  const ready = { callback: null };
  const interval = { callback: null };
  const startup = createStartup({
    appendLogs: () => calls.push("logs"),
    bindEvents: () => calls.push("bind"),
    focusSession: (id) => calls.push(`focus:${id}`),
    handleDaemonEvent: () => calls.push("event"),
    listen: async (name, callback) => {
      listeners.set(name, callback);
      calls.push(`listen:${name}`);
    },
    loadExternal: async () => calls.push("external"),
    loadPreflight: async () => calls.push("preflight"),
    loadPresets: async () => calls.push("presets"),
    loadSnapshot: async () => calls.push("snapshot"),
    localizeDom: () => calls.push("localize"),
    renderRows: () => calls.push("rows"),
    setupTerminal: () => calls.push("terminal"),
    showToast: (message) => calls.push(`toast:${message}`),
    startPinnedPolling: () => calls.push("pinned"),
    syncAgentFields: () => calls.push("agent-fields"),
    t: (key) => key,
    updateTerminalHeader: () => calls.push("header"),
    window: {
      addEventListener: (_name, callback) => {
        ready.callback = callback;
      },
    },
    wdioBuild,
    setInterval: (callback) => {
      interval.callback = callback;
    },
  });
  return { calls, interval, listeners, ready, startup };
}

test("startup registers streams, loads the initial state, and refreshes the shell", async () => {
  const harness = startupHarness();
  await harness.startup.startWhenReady();

  assert.deepEqual(harness.calls.slice(0, 5), ["localize", "terminal", "pinned", "bind", "agent-fields"]);
  assert.deepEqual([...harness.listeners.keys()], [
    "terminalai:event",
    "terminalai:logs",
    "terminalai:focus-session",
  ]);
  assert.deepEqual(harness.calls.slice(-4), ["preflight", "snapshot", "presets", "external"]);
  harness.interval.callback();
  assert.deepEqual(harness.calls.slice(-2), ["rows", "header"]);

  harness.listeners.get("terminalai:event")({ payload: {} });
  harness.listeners.get("terminalai:logs")({ payload: {} });
  harness.listeners.get("terminalai:focus-session")({ payload: { id: "session-7" } });
  assert.deepEqual(harness.calls.slice(-3), ["event", "logs", "focus:session-7"]);
});

test("WDIO startup waits for its explicit readiness event", async () => {
  const harness = startupHarness({ wdioBuild: true });
  let finished = false;
  const pending = harness.startup.startWhenReady().then(() => {
    finished = true;
  });
  await Promise.resolve();
  assert.equal(finished, false);
  assert.equal(harness.calls.length, 0);
  harness.ready.callback();
  await pending;
  assert.equal(finished, true);
  assert.equal(harness.calls[0], "localize");
});
