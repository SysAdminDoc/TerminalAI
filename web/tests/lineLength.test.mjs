import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

// A 2,204-character stylesheet line is a file in which a typo cannot be seen.
// That is not hypothetical here: a literal `\n` inside one of those lines took
// out the Review view's only layout, and review could not have caught it. These
// limits exist so the next one is visible in a diff.
const CSS_LIMIT = 100;
const JS_LIMIT = 120;

/**
 * main.js still carries long lines, almost all of them single-expression HTML
 * templates. They are tracked as a ratchet rather than reformatted in one pass:
 * breaking a template literal changes the string it produces, so the fix is to
 * extract those renderers into modules — as the fleet row was, into
 * rowMarkup.js — not to rewrap them mechanically. What this number guarantees is
 * that the debt cannot grow while the rest is pending. It only ever goes down.
 */
const MAIN_JS_LONG_LINE_BUDGET = 54;

const dir = new URL("../src/", import.meta.url);
const files = readdirSync(dir).filter((name) => name.endsWith(".css") || name.endsWith(".js"));

/** Lines over `limit` columns, as `line number: length` for a readable failure. */
function overLimit(name, limit) {
  return readFileSync(new URL(name, dir), "utf8")
    .split(/\r?\n/)
    .map((line, index) => ({ at: index + 1, length: line.length }))
    .filter((line) => line.length > limit)
    .map((line) => `${name}:${line.at} is ${line.length}`);
}

test("no stylesheet line is too long to review", () => {
  const offenders = files.filter((name) => name.endsWith(".css")).flatMap((name) => overLimit(name, CSS_LIMIT));
  assert.deepEqual(offenders, [], `stylesheet lines over ${CSS_LIMIT} columns`);
});

test("every frontend module except main.js is within the column limit", () => {
  const offenders = files
    .filter((name) => name.endsWith(".js") && name !== "main.js")
    .flatMap((name) => overLimit(name, JS_LIMIT));
  assert.deepEqual(offenders, [], `module lines over ${JS_LIMIT} columns`);
});

test("main.js does not accumulate more long lines than it already has", () => {
  const offenders = overLimit("main.js", JS_LIMIT);
  assert.ok(
    offenders.length <= MAIN_JS_LONG_LINE_BUDGET,
    `main.js has ${offenders.length} lines over ${JS_LIMIT} columns, budget is ${MAIN_JS_LONG_LINE_BUDGET}.\n` +
      "Extract the renderer into a module rather than raising this number.",
  );
});

test("the stylesheet is one declaration per line, not one rule per line", () => {
  // The shape the limit is protecting: a rule body whose declarations share a
  // line hides the one that is wrong.
  const css = readFileSync(new URL("styles.css", dir), "utf8");
  const packed = css
    .split(/\r?\n/)
    .map((line, index) => ({ at: index + 1, line }))
    .filter(({ line }) => (line.match(/;/g) ?? []).length > 1 && !line.trim().startsWith("/*"));
  assert.deepEqual(
    packed.map(({ at }) => at),
    [],
    "these lines carry more than one declaration",
  );
});
