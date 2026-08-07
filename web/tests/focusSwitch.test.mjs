import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();

test("focus switches serialize channel registration and restore the prior route", () => {
  assert.match(main, /focusQueue: Promise\.resolve\(\),/);
  assert.match(main, /const switchPromise = state\.focusQueue\.then\(\(\) => focusSessionNow\(id\)\);/);
  assert.match(main, /state\.focusQueue = switchPromise\.catch\(\(\) => \{\}\);/);

  const start = main.indexOf("async function focusSessionNow");
  const end = main.indexOf("\n/**", start);
  assert.ok(start >= 0 && end > start, "focus switch implementation is present");
  const body = main.slice(start, end);
  const staleGuard = body.indexOf("if (state.focused !== id) return;");
  const rollback = body.indexOf("state.focused = previousFocused;");
  assert.ok(staleGuard >= 0 && staleGuard < rollback, "stale failures are ignored before rollback");
  assert.match(body, /state\.focusGeneration \+= 1;\s*state\.outputChannel = null;/);
  assert.match(body, /if \(previousFocused\) await attachSessionOutput\(previousFocused\);/);
});
