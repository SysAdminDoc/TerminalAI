import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const styles = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");

const sliceBetween = (start, end) => main.slice(main.indexOf(start), main.indexOf(end));

test("project discovery failures remain distinct from an empty project list", () => {
  const open = sliceBetween("async function openProjects", "/** The queue button's glyph");
  const render = sliceBetween("function renderProjects", "async function openProjects");

  assert.match(open, /state\.projectsError = null;/);
  assert.match(open, /state\.projectsError = String\(error\);/);
  assert.doesNotMatch(open, /showToast\(String\(error\)\)/);
  assert.match(render, /if \(state\.projectsError\)/);
  assert.match(render, /projects-load-error/);
  assert.match(render, /renderDataError/);
  assert.match(render, /openProjects/);
  assert.ok(render.indexOf("projectsError") < render.indexOf("projects-none-registered"));
});

test("queue lookup failures do not render an empty queue", () => {
  const refresh = sliceBetween("async function refreshQueue", "function renderQueue");
  const render = sliceBetween("function renderQueue", "async function queueRowAction");

  assert.match(refresh, /state\.queueError = null;/);
  assert.match(refresh, /state\.queueError = String\(error\);/);
  assert.doesNotMatch(refresh, /showToast\(String\(error\)\)/);
  assert.match(render, /if \(state\.queueError\)/);
  assert.match(render, /queue-load-error/);
  assert.match(render, /queue-unavailable/);
  assert.match(render, /refreshQueue/);
  assert.ok(render.indexOf("queueError") < render.indexOf("queue-empty"));
});

test("review lookup failures replace the blank review with an alert and retry", () => {
  const load = sliceBetween("async function loadReview", "function setReviewMode");
  const render = sliceBetween("function renderReview", "function renderReviewEntry");

  assert.match(load, /state\.reviewError = null;/);
  assert.match(load, /state\.reviewError = String\(error\);/);
  assert.doesNotMatch(load, /showToast\(/);
  assert.match(render, /if \(state\.reviewError\)/);
  assert.match(render, /review-load-error/);
  assert.match(render, /renderDataError/);
  assert.match(render, /loadReview/);
  assert.ok(render.indexOf("reviewError") < render.indexOf("const entries"));
});

test("all three durable errors escape backend text and expose a localized retry", () => {
  const helper = sliceBetween("function renderDataError", "function invokeArgs");
  assert.match(helper, /escapeHtml\(message\)/);
  assert.match(helper, /data-retry-action/);
  assert.match(helper, /t\("button-retry"\)/);
  for (const key of [
    "button-retry",
    "projects-unavailable",
    "projects-load-error",
    "queue-unavailable",
    "queue-load-error",
    "review-unavailable",
    "review-load-error",
  ]) {
    assert.match(ftl, new RegExp(`^${key} = `, "m"), `${key} is missing from the catalog`);
  }
  assert.match(styles, /\.data-error/);
});
