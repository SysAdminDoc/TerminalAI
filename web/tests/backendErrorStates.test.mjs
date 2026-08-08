import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();
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

// Four of these render from state the window already holds, so they have no
// loading state and correctly never had one. They had no error state either: a
// renderer that throws left an open dialog with an empty body, which reads as
// "still loading" and never stops reading that way.
test("every state-less dialog renders its own failure rather than nothing", () => {
  const guard = sliceBetween("function renderGuarded", "function invokeArgs");
  assert.match(guard, /catch \(error\)/);
  assert.match(guard, /renderDataError\(container/);
  // Logged as well as shown: the message is for the operator, the stack is for
  // whoever has to find out why.
  assert.match(guard, /console\.error/);

  for (const [opener, container, key] of [
    ["openRollup", "rollup-body", "rollup-render-error"],
    ["openBroadcast", "broadcast-list", "broadcast-render-error"],
    ["openApprovals", "approvals-body", "approvals-render-error"],
    ["openExplainer", "explainer-states", "explainer-render-error"],
  ]) {
    // Sliced forward from the opener, not with a global indexOf: `sliceBetween`
    // finds the first closing brace in the whole file, which is above all of
    // these and would silently read nothing.
    const from = main.indexOf(`function ${opener}() {`);
    assert.notEqual(from, -1, `${opener} is gone; this test is reading nothing`);
    const open = main.slice(from, main.indexOf("\n}\n", from));
    assert.ok(open.includes("renderGuarded("), `${opener} does not guard its render`);
    assert.ok(open.includes(container), `${opener} names no container`);
    assert.ok(open.includes(key), `${opener} has no message`);
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} has no string`);
    // The dialog opens before the render, so a throw is visible in an open
    // dialog rather than swallowed by one that never appeared.
    assert.ok(
      open.indexOf("showModal") < open.indexOf("renderGuarded"),
      `${opener} renders before it opens, so a failure would render into a closed dialog`,
    );
  }
});

test("a fleet search that fails offers to run again", () => {
  // The one of the five with a real backend behind it, so a failure is often
  // transient and re-running is exactly what the operator would do -- and had
  // to do by retyping.
  const search = sliceBetween("async function runFleetSearch", "async function openSessionHistory");
  assert.match(search, /renderDataError\(\s*body,/);
  assert.match(search, /"runFleetSearch",\s*runFleetSearch,/);
  assert.ok(/^fleet-search-error = /m.test(ftl));
});
