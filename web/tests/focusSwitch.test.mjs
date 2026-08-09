import assert from "node:assert/strict";
import test from "node:test";
import { moduleSource } from "./appSource.mjs";

const shell = moduleSource("main.js");
const focus = moduleSource("sessionFocus.js");

test("focus switches serialize channel registration and restore the prior route", () => {
  assert.match(shell, /focusQueue: Promise\.resolve\(\),/);
  assert.match(focus, /const switchPromise = state\.focusQueue\.then\(\(\) => focusSessionNow\(id\)\);/);
  assert.match(focus, /state\.focusQueue = switchPromise\.catch\(\(\) => \{\}\);/);

  const start = focus.indexOf("async function focusSessionNow");
  const end = focus.indexOf("\n  return", start);
  assert.ok(start >= 0 && end > start, "focus switch implementation is present");
  const body = focus.slice(start, end);
  const staleGuard = body.indexOf("if (state.focused !== id) return;");
  const rollback = body.indexOf("state.focused = previousFocused;");
  assert.ok(staleGuard >= 0 && staleGuard < rollback, "stale failures are ignored before rollback");
  assert.match(body, /state\.focusGeneration \+= 1;\s*state\.outputChannel = null;/);
  assert.match(body, /if \(previousFocused\) await attachSessionOutput\(previousFocused\);/);
});
