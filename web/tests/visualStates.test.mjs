import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { cssSource } from "./cssSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const coordinator = readFileSync(
  new URL("../src/snapshotCoordinator.js", import.meta.url),
  "utf8",
).replace(/\r\n/g, "\n");
const fleetList = readFileSync(
  new URL("../src/fleetList.js", import.meta.url),
  "utf8",
).replace(/\r\n/g, "\n");
const styles = cssSource();
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("the narrow status label overrides the compact hide rule", () => {
  assert.match(
    styles,
    /@media \(max-width: 1180px\) \{[\s\S]*?\.row-folder \.row-status-label \{\s*display: inline;\s*\}/,
  );
});

test("the empty state stays hidden until the first snapshot finishes", () => {
  assert.match(html, /id="empty-state" class="empty-state empty-state-hidden"/);
  assert.match(
    fleetList,
    /classList\.toggle\(\s*"empty-state-hidden",\s*state\.snapshotLoading \|\| state\.sessions\.length > 0,?\s*\)/,
  );
  const load = coordinator.slice(coordinator.indexOf("async function loadSnapshotNow"));
  assert.ok(load.indexOf("state.snapshotLoading = false;") < load.indexOf("renderRows();"), "snapshot completion must precede empty-state rendering");
});

test("the identity column names only the fields visible in each row mode", () => {
  assert.match(html, /id="column-identity-label"[^>]*data-i18n="column-label-compact">STATUS \/ REPO/);
  assert.match(
    fleetList,
    /const identityLabelKey = state\.wideMode\s*\?\s*"column-label-wide"\s*:\s*"column-label-compact";/,
  );
  assert.match(ftl, /^column-label-compact = STATUS \/ REPO$/m);
  assert.match(ftl, /^column-label-wide = STATUS \/ REPO · BRANCH$/m);
});
