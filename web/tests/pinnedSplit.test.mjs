import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registrySource } from "./registrySource.mjs";
import test from "node:test";
import { appSource } from "./appSource.mjs";
import { cssSource } from "./cssSource.mjs";

const main = appSource();
const pinned = readFileSync(
  new URL("../src/pinnedPanes.js", import.meta.url),
  "utf8",
).replace(/\r\n/g, "\n");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const css = cssSource();
const registry = registrySource();

test("pinned panes render from Rust grids, not from more xterm instances", () => {
  // One renderer is what lets the fleet hold ~29 rows. Three more would undo
  // the entire premise of the row/pane split.
  assert.match(pinned, /invoke\("grid_snapshot", \{ id: session\.id \}\)/);
  const news = [...main.matchAll(/new Terminal\(/g)];
  assert.equal(news.length, 1, `exactly one xterm may be constructed, found ${news.length}`);
});

test("the pane limit matches the number the daemon actually enforces", () => {
  // Two constants that must agree; if they drift, the UI offers a fourth pin
  // the daemon then refuses.
  assert.match(pinned, /const MAX_PINNED = 3;/);
  assert.match(registry, /at most three sessions may be pinned/);
  assert.match(pinned, /\.slice\(0, MAX_PINNED\)/);
});

test("the split view is hidden until something is pinned", () => {
  assert.match(html, /id="pinned-split"[^>]*hidden/);
  assert.match(pinned, /host\.hidden = pinned\.length === 0;/);
});

test("panes are reconciled by id rather than rebuilt", () => {
  // Rebuilding would scroll or blank a sibling pane every time one snapshot
  // arrived, and snapshots arrive once a second.
  const fn = pinned.slice(pinned.indexOf("function renderPinnedSplit"));
  const body = fn.slice(0, fn.indexOf("\n  /// Read each pinned session"));
  assert.match(body, /reconcileKeyedRows\(/);
  assert.match(body, /\(session\) => session\.id,/);
});

test("a pane with no snapshot yet says so rather than showing an empty box", () => {
  // An empty box reads as "this session printed nothing", which is a different
  // and wrong claim.
  assert.match(pinned, /grid\.textContent = snapshot \?\? t\("pinned-waiting"\);/);
});

test("unpinning drops the stored grid so a stale frame cannot reappear", () => {
  const fn = pinned.slice(pinned.indexOf("async function refreshPinnedGrids"));
  const body = fn.slice(0, fn.indexOf("\n  function startPinnedPolling"));
  assert.match(body, /const live = new Set\(pinned\.map\(\(session\) => session\.id\)\);/);
  assert.match(body, /state\.pinnedGrids\.delete\(id\);/);
});

test("a session that exits mid-poll does not raise a toast", () => {
  // The row already says it exited; a toast per second would be noise.
  const fn = pinned.slice(pinned.indexOf("async function refreshPinnedGrids"));
  const body = fn.slice(0, fn.indexOf("\n  function startPinnedPolling"));
  assert.match(body, /\} catch \{/);
  assert.doesNotMatch(body, /showToast/);
});

test("pinned panes poll slower than the focused terminal streams", () => {
  // A pinned pane is for noticing that something changed, not reading along.
  assert.match(pinned, /const PINNED_POLL_MS = (\d+);/);
  const interval = Number(pinned.match(/const PINNED_POLL_MS = (\d+);/)[1]);
  assert.ok(interval >= 500, `${interval}ms is too aggressive for a background pane`);
});

test("the split view is styled and scrolls its own overflow", () => {
  assert.match(css, /\.pinned-split \{[^}]*overflow: auto/);
  assert.match(css, /\.pinned-pane-grid \{[^}]*white-space: pre/);
});
