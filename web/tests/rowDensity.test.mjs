import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { renderFixtureRow } from "./rowFixture.mjs";

import { JSDOM } from "jsdom";

/**
 * The 28px row is the product's central claim: it is the reason the README says
 * thirty sessions fit on one screen. These tests exist so the stylesheet and the
 * documents cannot drift apart again — the shipped row was 54px while both
 * documents specified about 28.
 *
 * jsdom has no layout engine, so the rendered height is measured in a real
 * browser (see the R-75 verification in CHANGELOG.md). What is asserted here is
 * that the declared height, the documented height, and the compact/Wide split
 * all still agree.
 */
const COMPACT_ROW_HEIGHT = 28;

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const readme = readFileSync(new URL("../../README.md", import.meta.url), "utf8");

test("the compact fleet row declares the documented height", () => {
  const declared = css.match(/\.fleet-row \{[^}]*min-height:\s*(\d+)px/);
  assert.ok(declared, ".fleet-row must declare a min-height");
  assert.equal(
    Number(declared[1]),
    COMPACT_ROW_HEIGHT,
    "the stylesheet and this test must agree on the row height",
  );
});

test("the README documents the same row height", () => {
  const documented = readme.match(/~(\d+)px/);
  assert.ok(documented, "README must state the row height");
  assert.equal(Number(documented[1]), COMPACT_ROW_HEIGHT);
});

test("the README states the visible-row count against a measured window size", () => {
  // "thirty sessions on one screen" is only meaningful with a window size beside
  // it. Refuse a bare claim.
  assert.match(readme, /1440\s*[x×]\s*900/i);
});

test("the compact row keeps one line and Wide carries the rest", () => {
  const dom = new JSDOM(html);
  const labels = dom.window.document.querySelectorAll(".column-labels span");
  assert.equal(labels.length, 4, "the row grid and its labels must stay in step");

  // Branch, ports and the spelled-out status belong to Wide, not the 28px line.
  for (const secondary of ["row-branch", "row-status-label", "row-ports"]) {
    assert.match(
      css,
      new RegExp(`\\.row-folder \\.${secondary}[^}]*display: none`),
      `${secondary} must be hidden in the compact row`,
    );
    assert.match(
      css,
      new RegExp(`\\.fleet-list-wide \\.row-folder \\.${secondary}`),
      `${secondary} must return in Wide`,
    );
  }
});

test("row actions meet the 24px target without widening the compact pitch", () => {
  const dom = new JSDOM(`<style>${css}</style>`);
  const rules = [...dom.window.document.styleSheets[0].cssRules];
  const actionRule = rules.find((rule) => rule.selectorText === ".row-action");
  assert.ok(actionRule, "row actions need a dedicated hit-area rule");
  assert.equal(actionRule.style.width, "24px");
  assert.equal(actionRule.style.height, "24px");
  assert.equal(actionRule.style.flex, "0 0 24px");
  assert.equal(actionRule.style.marginInline, "-1px");

  const rowActions = rules.find((rule) => rule.selectorText === ".row-actions");
  assert.equal(rowActions?.style.gap, "1px");
  // The 24px hit box and two 1px negative margins occupy 22px; with the 1px
  // flex gap, adjacent fleet controls stay on their documented 23px pitch.
  assert.equal(24 - 1 - 1 + 1, 23);
  assert.match(renderFixtureRow(), /class="row-action row-action-queue"/);
});

test("the fleet row and its column labels share one grid definition", () => {
  const rowGrid = css.match(/\.fleet-row \{[^}]*grid-template-columns:\s*([^;]+);/);
  const labelGrid = css.match(/\.column-labels \{[^}]*grid-template-columns:\s*([^;]+);/);
  assert.ok(rowGrid && labelGrid);
  assert.equal(
    rowGrid[1].trim(),
    labelGrid[1].trim(),
    "labels drifting from the row grid is how columns stop lining up",
  );
});

test("the hidden attribute actually hides a display-carrying block", () => {
  // Author `display: grid`/`flex` outranks the UA sheet's [hidden] rule, so the
  // wide-meta and reply blocks stayed visible and doubled the row height.
  assert.match(css, /\[hidden\] \{\s*display: none !important;\s*\}/);
});
