import assert from "node:assert/strict";
import test from "node:test";

import { JSDOM } from "jsdom";
import { createShellModes } from "../src/shellModes.js";

test("resetting the terminal clears output and updates the shell status", () => {
  const dom = new JSDOM('<output id="terminal-status"></output>');
  const { document } = dom.window;
  const state = { terminal: { reset: () => resets++ }, preflightMode: false, reviewMode: false };
  let resets = 0;
  let headerUpdates = 0;
  const modes = createShellModes({
    $: (id) => document.getElementById(id),
    loadReview: async () => {},
    renderRows: () => {},
    setPreflightMode: () => {},
    state,
    syncReviewVisibility: () => {},
    t: (key) => key,
    updateTerminalHeader: () => headerUpdates++,
  });

  modes.resetTerminal("No active session");

  assert.equal(resets, 1);
  assert.equal(document.getElementById("terminal-status").textContent, "No active session");
  assert.equal(headerUpdates, 1);
});

test("entering review mode exits preflight and loads review rows", async () => {
  const state = { terminal: null, preflightMode: true, reviewMode: false };
  let preflightChanges = 0;
  let visibilityChanges = 0;
  let reviewLoads = 0;
  let rowRenders = 0;
  const modes = createShellModes({
    $: () => null,
    loadReview: async () => reviewLoads++,
    renderRows: () => rowRenders++,
    setPreflightMode: (active) => {
      preflightChanges++;
      state.preflightMode = active;
    },
    state,
    syncReviewVisibility: () => visibilityChanges++,
    t: (key) => key,
    updateTerminalHeader: () => {},
  });

  modes.setReviewMode(true);
  await Promise.resolve();

  assert.equal(state.reviewMode, true);
  assert.equal(state.preflightMode, false);
  assert.equal(preflightChanges, 1);
  assert.equal(visibilityChanges, 1);
  assert.equal(reviewLoads, 1);
  assert.equal(rowRenders, 0);

  modes.setReviewMode(false);
  assert.equal(rowRenders, 1);
});
