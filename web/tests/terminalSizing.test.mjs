import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { moduleSource } from "./appSource.mjs";

const main = moduleSource("main.js");
const pane = moduleSource("terminalPane.js");
const layout = moduleSource("terminalLayout.js");
const focus = moduleSource("sessionFocus.js");
const history = moduleSource("terminalHistory.js");
const output = moduleSource("rendererUtils.js");

/**
 * The fit addon was constructed and registered but never called, and there was
 * no resize listener, so the grid stayed at a hard-coded 120x40 no matter how
 * large the pane was. Measured in a real browser after the fix: 99 columns at a
 * 1356px window, 141 at 1920px, 74 with the panel narrowed — see CHANGELOG.md.
 */
test("the fit addon is actually driven, not just registered", () => {
  assert.match(layout, /state\.fitAddon\.proposeDimensions\(\)/);
  assert.match(pane, /observeTerminalSize\(\)/);
  assert.match(pane, /new ResizeObserver\(scheduleFit\)/);
  assert.match(pane, /window\.addEventListener\("resize", scheduleFit\)/);
});

test("resize is debounced and never fired per drag frame", () => {
  // Agent TUIs hard-wrap and do not reflow, so a resize arriving mid-drag
  // corrupts the output the supervisor parses for status.
  assert.match(layout, /const RESIZE_DEBOUNCE_MS = \d+;/);
  const debounce = Number(layout.match(/const RESIZE_DEBOUNCE_MS = (\d+);/)[1]);
  assert.ok(debounce >= 100, `debounce of ${debounce}ms is too tight for a splitter drag`);
  assert.match(layout, /if \(state\.resizeTimer\) clearTimeout\(state\.resizeTimer\)/);
});

test("an unchanged size is not resent to the pty", () => {
  assert.match(layout, /if \(state\.lastSentSize === signature\) return;/);
});

test("the same geometry is sent separately for each focused session", () => {
  // A single global `cols x rows` signature lets session B inherit session A's
  // size when focus changes without changing the renderer dimensions.
  assert.match(layout, /const signature = `\$\{state\.focused\}:\$\{cols\}x\$\{rows\}`;/);
});

test("output for a previous session is discarded across a focus switch", () => {
  // state.focused is assigned before an await and output arrives asynchronously,
  // so a late chunk could otherwise be written into the wrong session's grid.
  assert.match(main, /focusGeneration: 0,/);
  assert.match(focus, /state\.focusGeneration \+= 1;/);
  assert.match(
    output,
    /if \(id !== state\.focused \|\| generation !== state\.focusGeneration\) return;/,
  );
  // The channel captures the generation at creation time, not at delivery time.
  assert.match(history, /const generation = state\.focusGeneration;/);
});

test("nothing hard-codes the grid at 120x40 any more", () => {
  const terminal = `${layout}\n${pane}`;
  assert.doesNotMatch(terminal, /resize_session", \{ id: state\.focused, rows: 40, cols: 120 \}/);
  assert.doesNotMatch(terminal, /state\.terminal\?\.resize\(120, 40\)/);
  // The one remaining 120x40 is the pre-fit default, named rather than inline.
  assert.match(layout, /const DEFAULT_COLS = 120;/);
  assert.match(pane, /state\.terminal\.resize\(DEFAULT_COLS, DEFAULT_ROWS\)/);
});
