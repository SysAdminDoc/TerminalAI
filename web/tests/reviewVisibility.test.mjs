import assert from "node:assert/strict";
import test from "node:test";

import { JSDOM } from "jsdom";
import { createReviewVisibility } from "../src/reviewVisibility.js";

function visibilityDom() {
  return new JSDOM(`
    <div id="fleet-state-strip"></div>
    <div id="column-labels"></div>
    <div id="fleet-list"></div>
    <div id="empty-state"></div>
    <div id="review-view"></div>
    <button id="review-toggle"></button>
  `);
}

test("review visibility hides the fleet and exposes review mode", () => {
  const dom = visibilityDom();
  const { document } = dom.window;
  const state = { reviewMode: true, preflightMode: false };
  const visibility = createReviewVisibility({
    $: (id) => document.getElementById(id),
    state,
    t: (key) => key,
  });

  visibility.syncReviewVisibility();

  for (const id of ["fleet-state-strip", "column-labels", "fleet-list", "empty-state"]) {
    assert.equal(document.getElementById(id).classList.contains("view-hidden"), true);
  }
  assert.equal(document.getElementById("review-view").classList.contains("view-hidden"), false);
  assert.equal(document.getElementById("review-toggle").getAttribute("aria-pressed"), "true");
  assert.equal(document.getElementById("review-toggle").textContent, "button-fleet");
});

test("preflight keeps both working surfaces hidden and restores the review label", () => {
  const dom = visibilityDom();
  const { document } = dom.window;
  const state = { reviewMode: true, preflightMode: true };
  const visibility = createReviewVisibility({
    $: (id) => document.getElementById(id),
    state,
    t: (key) => key,
  });

  visibility.syncReviewVisibility();

  assert.equal(document.getElementById("review-view").classList.contains("view-hidden"), true);
  assert.equal(document.getElementById("review-toggle").classList.contains("wide-toggle-active"), false);
  assert.equal(document.getElementById("review-toggle").textContent, "button-review");
});
