import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();

/**
 * The fit addon was constructed and registered but never called, and there was
 * no resize listener, so the grid stayed at a hard-coded 120x40 no matter how
 * large the pane was. Measured in a real browser after the fix: 99 columns at a
 * 1356px window, 141 at 1920px, 74 with the panel narrowed — see CHANGELOG.md.
 */
test("the fit addon is actually driven, not just registered", () => {
  assert.match(main, /state\.fitAddon\.proposeDimensions\(\)/);
  assert.match(main, /observeTerminalSize\(\)/);
  assert.match(main, /new ResizeObserver\(scheduleFit\)/);
  assert.match(main, /window\.addEventListener\("resize", scheduleFit\)/);
});

test("resize is debounced and never fired per drag frame", () => {
  // Agent TUIs hard-wrap and do not reflow, so a resize arriving mid-drag
  // corrupts the output the supervisor parses for status.
  assert.match(main, /const RESIZE_DEBOUNCE_MS = \d+;/);
  const debounce = Number(main.match(/const RESIZE_DEBOUNCE_MS = (\d+);/)[1]);
  assert.ok(debounce >= 100, `debounce of ${debounce}ms is too tight for a splitter drag`);
  assert.match(main, /if \(state\.resizeTimer\) clearTimeout\(state\.resizeTimer\)/);
});

test("an unchanged size is not resent to the pty", () => {
  assert.match(main, /if \(state\.lastSentSize === signature\) return;/);
});

test("the same geometry is sent separately for each focused session", () => {
  // A single global `cols x rows` signature lets session B inherit session A's
  // size when focus changes without changing the renderer dimensions.
  assert.match(main, /const signature = `\$\{state\.focused\}:\$\{cols\}x\$\{rows\}`;/);
});

test("output for a previous session is discarded across a focus switch", () => {
  // state.focused is assigned before an await and output arrives asynchronously,
  // so a late chunk could otherwise be written into the wrong session's grid.
  assert.match(main, /focusGeneration: 0,/);
  assert.match(main, /state\.focusGeneration \+= 1;/);
  assert.match(
    main,
    /if \(id !== state\.focused \|\| generation !== state\.focusGeneration\) return;/,
  );
  // The channel captures the generation at creation time, not at delivery time.
  assert.match(main, /const generation = state\.focusGeneration;/);
});

test("nothing hard-codes the grid at 120x40 any more", () => {
  assert.doesNotMatch(main, /resize_session", \{ id: state\.focused, rows: 40, cols: 120 \}/);
  assert.doesNotMatch(main, /state\.terminal\?\.resize\(120, 40\)/);
  // The one remaining 120x40 is the pre-fit default, named rather than inline.
  assert.match(main, /const DEFAULT_COLS = 120;/);
  assert.match(main, /state\.terminal\.resize\(DEFAULT_COLS, DEFAULT_ROWS\)/);
});
