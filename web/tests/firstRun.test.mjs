import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  createFirstRunDemoSessions,
  demoStatusCount,
  FIRST_RUN_DEMO_PREFIX,
  FIRST_RUN_STEP_IDS,
  normalizeFirstRunProgress,
  readFirstRunProgress,
  saveFirstRunProgress,
} from "../src/firstRun.js";
import { appSource } from "./appSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const source = appSource();

const expectedStatuses = [
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
];

function memoryStorage() {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
}

test("the first-run demo covers every fleet state with frontend-only rows", () => {
  const sessions = createFirstRunDemoSessions(1_700_000_000_000);
  assert.deepEqual(sessions.map((session) => session.status), expectedStatuses);
  assert.equal(demoStatusCount(sessions), expectedStatuses.length);
  assert.ok(sessions.every((session) => session.id.startsWith(FIRST_RUN_DEMO_PREFIX)));
  assert.ok(sessions.every((session) => session.cwd.endsWith("terminalai-demo")));
  assert.ok(sessions.every((session) => session.resume_id === null || session.resume_id.startsWith(FIRST_RUN_DEMO_PREFIX)));
});

test("first-run progress is bounded to the local checklist schema", () => {
  assert.deepEqual(normalizeFirstRunProgress({ project: true, demo: "yes", other: true }), {
    project: true,
    demo: false,
    launcher: false,
  });
  assert.deepEqual(FIRST_RUN_STEP_IDS, ["project", "demo", "launcher"]);
});

test("first-run progress survives only through the caller's local storage", () => {
  const storage = memoryStorage();
  const saved = saveFirstRunProgress({ project: true, launcher: true }, storage);
  assert.deepEqual(readFirstRunProgress(storage), saved);
  assert.equal(storage.getItem("terminalai.first-run.v1").includes("demo"), true);
});

test("the shell exposes the guided path and keeps demo focus offline", () => {
  for (const id of ["empty-demo-button", "first-run-checklist", "demo-mode-banner", "demo-exit-button"]) {
    assert.match(html, new RegExp(`id="${id}"`), `${id} is missing from the shell`);
  }
  assert.match(source, /if \(state\.demoMode && isFirstRunDemoSession\(id\)\)/);
  assert.match(source, /if \(!state\.focused \|\| state\.demoMode\) return;/);
  assert.match(source, /onFirstRunStep\?\.\("project"\)/);
  assert.match(source, /onFirstRunStep\?\.\("launcher"\)/);
});
