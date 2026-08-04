import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const styles = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("the narrow status label overrides the compact hide rule", () => {
  assert.match(
    styles,
    /@media \(max-width: 1180px\) \{[\s\S]*?\.row-folder \.row-status-label \{\s*display: inline;\s*\}/,
  );
});

test("the empty state stays hidden until the first snapshot finishes", () => {
  assert.match(html, /id="empty-state" class="empty-state empty-state-hidden"/);
  assert.match(main, /classList\.toggle\("empty-state-hidden", state\.snapshotLoading \|\| state\.sessions\.length > 0\)/);
  const load = main.slice(main.indexOf("async function loadSnapshotNow"), main.indexOf("\nasync function focusSession"));
  assert.ok(load.indexOf("state.snapshotLoading = false;") < load.indexOf("renderRows();"), "snapshot completion must precede empty-state rendering");
});

test("the identity column names only the fields visible in each row mode", () => {
  assert.match(html, /id="column-identity-label"[^>]*data-i18n="column-label-compact">STATUS \/ REPO/);
  assert.match(main, /const identityLabelKey = state\.wideMode \? "column-label-wide" : "column-label-compact";/);
  assert.match(ftl, /^column-label-compact = STATUS \/ REPO$/m);
  assert.match(ftl, /^column-label-wide = STATUS \/ REPO · BRANCH$/m);
});
