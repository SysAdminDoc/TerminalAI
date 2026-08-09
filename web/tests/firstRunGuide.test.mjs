import assert from "node:assert/strict";
import test from "node:test";

import { JSDOM } from "jsdom";
import { createFirstRunGuide } from "../src/firstRunGuide.js";

function guideDom() {
  return new JSDOM(`
    <section id="first-run-checklist">
      <div data-first-run-step="project"><span class="first-run-step-state"></span></div>
      <div data-first-run-step="demo"><span class="first-run-step-state"></span></div>
      <div data-first-run-step="launcher"><span class="first-run-step-state"></span></div>
    </section>
    <output id="first-run-progress"></output>
  `);
}

test("first-run guide renders progress and localized step state", () => {
  const dom = guideDom();
  const { document } = dom.window;
  const state = { firstRunProgress: { project: true, demo: false, launcher: false } };
  const guide = createFirstRunGuide({
    $: (id) => document.getElementById(id),
    state,
    t: (key, values) => values ? `${key}:${values.done}/${values.total}` : key,
  });

  guide.renderFirstRunGuide();

  assert.equal(document.getElementById("first-run-progress").textContent, "first-run-progress:1/3");
  assert.equal(document.querySelector('[data-first-run-step="project"]').dataset.complete, "true");
  assert.equal(document.querySelector('[data-first-run-step="project"] .first-run-step-state').textContent, "first-run-step-done");
  assert.equal(document.querySelector('[data-first-run-step="demo"] .first-run-step-state').textContent, "first-run-step-next");
});

test("marking a first-run step saves the normalized progress and rerenders", () => {
  const dom = guideDom();
  const { document } = dom.window;
  const state = { firstRunProgress: { project: false, demo: false, launcher: false } };
  const saved = [];
  const guide = createFirstRunGuide({
    $: (id) => document.getElementById(id),
    saveProgress: (progress) => {
      saved.push(progress);
      return progress;
    },
    state,
    t: (key) => key,
  });

  guide.markFirstRunStep("demo");

  assert.deepEqual(saved, [{ project: false, demo: true, launcher: false }]);
  assert.equal(state.firstRunProgress.demo, true);
  assert.equal(document.querySelector('[data-first-run-step="demo"]').dataset.complete, "true");
  guide.markFirstRunStep("demo");
  assert.equal(saved.length, 1);
});
