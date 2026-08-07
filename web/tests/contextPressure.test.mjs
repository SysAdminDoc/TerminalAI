import assert from "node:assert/strict";
import test from "node:test";

import {
  compactionCount,
  contextLabel,
  contextTitle,
  contextTone,
  tokenCount,
} from "../src/contextPressure.js";
import { renderFixtureRow } from "./rowFixture.mjs";

const t = (key, args) => (args ? `${key}:${JSON.stringify(args)}` : key);
const reading = (used, window, source = "agent") => ({
  context: { used_tokens: used, window_tokens: window, source },
});

test("a percentage is only shown when there is a real denominator", () => {
  // The rule the whole feature rests on: a window is reported, never guessed,
  // so a session whose agent does not report one shows what was measured and
  // nothing more.
  assert.equal(contextLabel(reading(42_000, 200_000)), "21%");
  assert.equal(contextLabel(reading(42_000, null, "transcript")), "42k");
  assert.equal(contextLabel({}), "—");
  assert.equal(contextLabel({ context: null }), "—");
});

test("a zero window is treated as no window rather than as a division", () => {
  // Mirrors ContextUsage::used_fraction. Infinity in a percentage field is the
  // worst possible reading of "the provider told us nothing".
  assert.equal(contextLabel(reading(1_000, 0)), "1k");
  assert.equal(contextTone(reading(1_000, 0)), "");
});

test("an unmeasured window is never toned as comfortable", () => {
  // Absence is not comfort. Painting it green would say the session is fine on
  // evidence we do not have.
  assert.equal(contextTone({}), "");
  assert.equal(contextTone(reading(180_000, null, "transcript")), "");
  assert.equal(contextTone(reading(10_000, 200_000)), "context-comfortable");
});

test("the tone bands match the core's thresholds and are inclusive", () => {
  const at = (used) => contextTone(reading(used, 100_000));
  assert.equal(at(74_999), "context-comfortable");
  assert.equal(at(75_000), "context-filling");
  assert.equal(at(89_999), "context-filling");
  assert.equal(at(90_000), "context-critical");
  assert.equal(at(200_000), "context-critical", "past full is still critical, not wrapped");
});

test("token counts stay readable at every magnitude", () => {
  assert.equal(tokenCount(0), "0");
  assert.equal(tokenCount(999), "999");
  assert.equal(tokenCount(41_000), "41k");
  assert.equal(tokenCount(1_200_000), "1.2M");
});

test("the hover text carries the numbers the cell cannot", () => {
  const explained = contextTitle(reading(42_000, 200_000), t);
  assert.match(explained, /^context-explained:/);
  assert.match(explained, /"used":"42,000"/);
  assert.match(explained, /"window":"200,000"/);
  assert.match(explained, /"percent":21/);

  const noWindow = contextTitle(reading(42_000, null, "transcript"), t);
  assert.match(noWindow, /^context-no-window:/, "says why there is no percentage");

  assert.match(contextTitle({}, t), /^context-unmeasured/);
});

test("compactions are reported wherever the reading is", () => {
  // A session that has compacted three times has explained pauses, whether or
  // not anything has measured its window since.
  assert.equal(compactionCount({ compactions: 3 }), 3);
  assert.equal(compactionCount({ compactions: 0 }), 0);
  assert.equal(compactionCount({}), 0);
  assert.equal(compactionCount({ compactions: "many" }), 0);

  const unmeasured = contextTitle({ compactions: 3 }, t);
  assert.match(unmeasured, /context-unmeasured/);
  assert.match(unmeasured, /context-compactions:\{"count":3\}/);
  assert.doesNotMatch(contextTitle({ compactions: 0 }, t), /context-compactions/);
});

test("the wide row carries a context cell that says what it measured", () => {
  const full = renderFixtureRow(reading(190_000, 200_000));
  assert.match(full, /<small>CTX<\/small><b data-row-context class="context-critical"/);
  assert.match(full, />95%<\/b>/);

  const derived = renderFixtureRow(reading(42_000, null, "transcript"));
  assert.match(derived, /<b data-row-context title=/, "no tone class without a window");
  assert.doesNotMatch(derived, /class="context-/);
  assert.match(derived, />42k<\/b>/);

  const unmeasured = renderFixtureRow({});
  assert.match(unmeasured, /<b data-row-context title="[^"]*context-unmeasured[^"]*">—<\/b>/);
});
