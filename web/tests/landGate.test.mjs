import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { moduleSource } from "./appSource.mjs";

const reviewPage = moduleSource("reviewPage.js");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const land = readFileSync(
  new URL("../../crates/terminalai-core/src/land.rs", import.meta.url),
  "utf8",
);

test("landing is offered only for a review that can be trusted", () => {
  // A timed-out or errored collection describes a tree nobody read; conflicts
  // are refused by the gate anyway. Offering the button there would teach the
  // operator that it sometimes does nothing.
  assert.match(
    reviewPage,
    /const canLand = !entry\.error && !entry\.timed_out && !conflicts\.length && files > 0;/,
  );
});

test("the landing request pins the commit the review was read against", () => {
  // Without this the moved-target refusal has nothing to compare and could
  // never fire — the exact class of check this codebase has had to fix before.
  assert.match(reviewPage, /expected_target_head: entry\.target_head \?\? null/);
  assert.match(
    land,
    /pub target_head: Option<String>|pub expected_target_head: Option<String>/,
  );
});

test("every refusal the gate can return has a message here", () => {
  // A refusal the renderer cannot name would surface as "nothing happened",
  // which is worse than the refusal itself.
  const kebab = (name) => name.replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase();
  const body = land.match(/^pub enum LandRefusal \{([\s\S]*?)^\}/m);
  assert.ok(body, "LandRefusal enum not found in land.rs");
  const declared = new Set(
    [...body[1].matchAll(/^ {4}([A-Z][A-Za-z]+)/gm)].map((match) => kebab(match[1])),
  );
  assert.ok(declared.size >= 8, `expected the refusal list, found ${[...declared]}`);
  for (const reason of declared) {
    assert.ok(
      new RegExp(`case "${reason}":`).test(reviewPage),
      `refusal "${reason}" has no branch in refusalText`,
    );
  }
});

test("an unknown refusal is still shown rather than swallowed", () => {
  const fn = reviewPage.slice(reviewPage.indexOf("function refusalText"));
  const body = fn.slice(0, fn.indexOf("\nasync function"));
  assert.match(body, /default:/);
  assert.match(body, /return String\(outcome\.reason \?\? outcome\);/);
});

test("a refusal stays on the review entry and the button becomes usable again", () => {
  const fn = reviewPage.slice(reviewPage.indexOf("async function landSession"));
  const body = fn.slice(0, fn.indexOf("\n/// Turn a structured refusal"));
  assert.match(body, /const reason = t\("review-land-refused"/);
  assert.match(body, /entry\.land_error = reason;/);
  assert.match(body, /renderReview\(\);/);
  assert.doesNotMatch(body, /showToast\(t\("review-land-refused"/);
  // finally, not the success path: a refused landing must not leave a dead
  // button behind.
  assert.match(body, /\} finally \{[\s\S]*button\.disabled = false;/);
});

test("the review renderer keeps landing refusals visible beside the diff", () => {
  const render = reviewPage.slice(reviewPage.indexOf("function renderReviewEntry"));
  assert.match(render, /const reviewError = entry\.error \|\| entry\.land_error;/);
  assert.match(render, /escapeHtml\(reviewError\)/);
  assert.match(render, /class="review-error" role="alert"/);
});

test("every land string the renderer uses exists in the catalog", () => {
  const used = [...reviewPage.matchAll(/t\("(land-[a-z-]+|review-land[a-z-]*|review-landing)"/g)].map(
    (match) => match[1],
  );
  assert.ok(used.length >= 8, `expected several, found ${used}`);
  for (const key of new Set(used)) {
    assert.ok(new RegExp(`^${key} =`, "m").test(ftl), `${key} missing from terminalai.ftl`);
  }
});

test("the renderer never resolves a landing itself", () => {
  // Nothing merges, stages, commits, or picks a side on the operator's behalf,
  // and the renderer must not add a path that does.
  assert.doesNotMatch(reviewPage, /invoke\("(git_|merge|resolve_conflict)/);
  assert.match(land, /Never auto-resolved/);
});
