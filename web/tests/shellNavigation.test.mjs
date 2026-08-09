import assert from "node:assert/strict";
import test from "node:test";

import { JSDOM } from "jsdom";
import { createShellNavigation } from "../src/shellNavigation.js";

function navigationDom() {
  const dom = new JSDOM(`
    <button class="rail-item" data-rail-page="projects" data-rail-target="projects-toggle"></button>
    <button class="rail-item" data-rail-page="fleet"></button>
    <button id="projects-toggle"></button>
    <dialog class="workspace-page" id="projects-dialog"></dialog>
    <dialog class="workspace-page" id="settings-dialog"></dialog>
  `);
  const { document } = dom.window;
  for (const dialog of document.querySelectorAll("dialog")) {
    dialog.show = () => {
      dialog.open = true;
      dialog.setAttribute("open", "");
    };
    dialog.close = () => {
      dialog.open = false;
      dialog.removeAttribute("open");
    };
  }
  return dom;
}

test("rail navigation marks the active page and delegates to existing controls", () => {
  const dom = navigationDom();
  const { document } = dom.window;
  const state = { preflightMode: false };
  let clicked = 0;
  document.getElementById("projects-toggle").addEventListener("click", () => clicked++);
  const navigation = createShellNavigation({
    $: (id) => document.getElementById(id),
    document,
    setPreflightMode: () => {},
    state,
  });

  navigation.wireRailNavigation();
  document.querySelector('[data-rail-page="projects"]').click();
  assert.equal(clicked, 1);
  assert.equal(document.querySelector('[data-rail-page="projects"]').getAttribute("aria-current"), "page");
  assert.equal(document.querySelector('[data-rail-page="fleet"]').hasAttribute("aria-current"), false);
});

test("opening a workspace page closes its sibling and exits preflight", () => {
  const dom = navigationDom();
  const { document } = dom.window;
  const projects = document.getElementById("projects-dialog");
  const settings = document.getElementById("settings-dialog");
  settings.show();
  const state = { preflightMode: true };
  let preflightChanges = 0;
  const navigation = createShellNavigation({
    $: (id) => document.getElementById(id),
    document,
    setPreflightMode: (active) => {
      preflightChanges++;
      state.preflightMode = active;
    },
    state,
  });

  navigation.openWorkspacePage(projects);
  assert.equal(settings.open, false);
  assert.equal(projects.open, true);
  assert.equal(preflightChanges, 1);
  assert.equal(state.preflightMode, false);
  assert.equal(document.querySelector('[data-rail-page="projects"]')?.getAttribute("aria-current"), "page");
});
