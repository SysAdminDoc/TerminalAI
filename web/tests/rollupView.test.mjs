import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";
import { cssSource } from "./cssSource.mjs";

const main = appSource();
const rollupPage = readFileSync(
  new URL("../src/rollupPage.js", import.meta.url),
  "utf8",
).replace(/\r\n/g, "\n");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const css = cssSource();

const renderRollup = (() => {
  const start = rollupPage.indexOf("function renderRollup()");
  return rollupPage.slice(start, rollupPage.indexOf("function openRollup", start));
})();

test("the fleet's spend figure is the way into the breakdown", () => {
  // A rollup nobody can find is a rollup nobody uses, and the aggregate is
  // exactly where the question "on what?" occurs to the operator.
  assert.match(main, /id="fleet-spend"/);
  assert.match(main, /\$\("fleet-summary"\)\.addEventListener\("click", \(event\) => \{/);
  assert.doesNotMatch(main, /\$\("fleet-spend"\)\?\.addEventListener/);
  assert.match(html, /id="rollup-dialog"/);
});

test("the entry point is a button, not a span with a handler", () => {
  // It is keyboard reachable and announced as a control only if it is one.
  // The class list gained a conditional modifier when the spend ceiling started
  // marking a blocking cap; the guarantee this asserts is the element type.
  assert.match(main, /<button type="button" class="summary-item summary-spend/);
  assert.match(main, /escapeHtml\(t\("button-open-rollup"\)\)/);
  assert.match(css, /\.summary-spend:focus-visible/);
});

test("every grouping the rollup promises is actually rendered", () => {
  for (const key of ["rollup-by-agent", "rollup-by-folder", "rollup-by-session", "rollup-total"]) {
    assert.ok(renderRollup.includes(key), `${key} not rendered`);
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} has no string`);
  }
});

test("coverage is stated on every render, not only when something is missing", () => {
  // A total with no statement of what it covers reads as the whole fleet.
  assert.match(renderRollup, /\$\("rollup-coverage"\)\.textContent = coverage\(totals, t\)/);
  for (const key of ["rollup-complete", "rollup-partial", "rollup-empty"]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} missing`);
  }
});

test("an unpriced group shows an em dash rather than a confident zero", () => {
  // A group whose sessions have no transcript read has not spent $0.00 — that
  // is a different claim, and the one the operator would act on.
  assert.match(renderRollup, /formatCost\(row\.priced \? row\.cost_usd : null\)/);
  assert.match(renderRollup, /formatCost\(totals\.priced \? totals\.cost_usd : null\)/);
});

test("session names and folders reaching the DOM are escaped", () => {
  // Every value that lands in a cell is operator-supplied (a session name) or
  // comes from a path on disk, and both reach here as raw strings.
  for (const value of ["label(row)", "t(titleKey)", "t(key)", "formatTokens(row[field])"]) {
    assert.ok(
      renderRollup.includes(`escapeHtml(${value})`),
      `${value} is interpolated without escapeHtml`,
    );
  }
  // The session grouping builds its label from the session's own name.
  assert.match(
    renderRollup,
    /label: session \? String\(session\.id\) \+ " · " \+ String\(session\.name\)/,
  );
  assert.match(renderRollup, /escapeHtml\(label\(row\)\)/);
});

test("a wide table scrolls inside itself rather than the dialog", () => {
  assert.match(css, /\.rollup-table \{[^}]*overflow-x: auto/);
});

test("token columns come from one list, so a new one cannot appear in the body only", () => {
  // The header and the cells are generated from the same source; adding a
  // token kind to one and not the other would shift every column silently.
  assert.match(renderRollup, /TOKEN_FIELDS\.map\(\(\[field\]\)/);
  assert.match(renderRollup, /TOKEN_FIELDS\.map\(\s*\(\[, key\]\)/);
});
