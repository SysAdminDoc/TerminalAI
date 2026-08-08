import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();
const coordinator = readFileSync(
  new URL("../src/snapshotCoordinator.js", import.meta.url),
  "utf8",
).replace(/\r\n/g, "\n");

test("snapshot refreshes replay events that arrived during the fetch", () => {
  assert.match(main, /snapshotQueue: Promise\.resolve\(\),/);
  assert.match(main, /snapshotEvents: \[\],/);
  assert.match(coordinator, /const snapshotPromise = state\.snapshotQueue\.then\(\(\) => loadSnapshotNow\(\)\);/);
  assert.match(main, /if \(state\.snapshotLoading\) state\.snapshotEvents\.push\(\{ kind: "session-updated", session \}\);/);
  assert.match(main, /if \(state\.snapshotLoading\) state\.snapshotEvents\.push\(\{ kind: "session-removed", id \}\);/);

  const start = coordinator.indexOf("async function loadSnapshotNow");
  assert.ok(start >= 0, "snapshot implementation is present");
  const body = coordinator.slice(start);
  const snapshotAssignment = body.indexOf("state.sessions = snapshot.sessions ?? [];");
  const replay = body.indexOf("for (const event of pendingEvents)");
  assert.ok(snapshotAssignment >= 0 && replay > snapshotAssignment, "buffered events replay after the snapshot");
  assert.match(body, /if \(event\.kind === "session-updated"\) applySessionUpdate\(event\.session, false\);/);
  assert.match(body, /if \(event\.kind === "session-removed"\) applySessionRemoval\(event\.id\);/);
  const loadingDone = body.indexOf("state.snapshotLoading = false;");
  const focusedAttach = body.indexOf("if (state.focused)", loadingDone);
  assert.ok(loadingDone >= 0 && focusedAttach > loadingDone, "loading must finish before the focused channel is attached");
});
