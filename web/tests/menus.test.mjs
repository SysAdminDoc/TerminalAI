import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

import { wireOverflowMenus } from "../src/menus.js";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");

/** The real index.html, wired exactly as the app wires it. */
function mount() {
  const dom = new JSDOM(html, { runScripts: "outside-only" });
  const doc = dom.window.document;
  wireOverflowMenus(doc);
  const id = (name) => doc.getElementById(name);
  const click = (element) => {
    const event = new dom.window.MouseEvent("click", { bubbles: true, cancelable: true });
    element.dispatchEvent(event);
  };
  const escape = () => {
    const event = new dom.window.KeyboardEvent("keydown", { key: "Escape", bubbles: true });
    doc.dispatchEvent(event);
  };
  return { dom, doc, id, click, escape };
}

test("both menus start closed", () => {
  const { id } = mount();
  assert.equal(id("app-menu").hidden, true);
  assert.equal(id("tools-menu").hidden, true);
  assert.equal(id("app-menu-button").getAttribute("aria-expanded"), "false");
  assert.equal(id("tools-menu-button").getAttribute("aria-expanded"), "false");
});

test("the trigger opens its menu and reports it to assistive tech", () => {
  const { id, click } = mount();
  click(id("tools-menu-button"));
  assert.equal(id("tools-menu").hidden, false);
  assert.equal(id("tools-menu-button").getAttribute("aria-expanded"), "true");
});

test("clicking the trigger again closes it", () => {
  const { id, click } = mount();
  click(id("tools-menu-button"));
  click(id("tools-menu-button"));
  assert.equal(id("tools-menu").hidden, true);
  assert.equal(id("tools-menu-button").getAttribute("aria-expanded"), "false");
});

test("opening one menu closes the other", () => {
  // Two panels open at once overlap each other in the chrome.
  const { id, click } = mount();
  click(id("app-menu-button"));
  click(id("tools-menu-button"));
  assert.equal(id("app-menu").hidden, true);
  assert.equal(id("tools-menu").hidden, false);
});

test("a click outside closes the open menu", () => {
  const { doc, id, click } = mount();
  click(id("tools-menu-button"));
  click(doc.body);
  assert.equal(id("tools-menu").hidden, true);
});

test("Escape closes the menu and returns focus to its trigger", () => {
  // Without the focus return, keyboard users are dropped at the top of the
  // document every time they dismiss a menu.
  const { doc, id, click, escape } = mount();
  click(id("tools-menu-button"));
  escape();
  assert.equal(id("tools-menu").hidden, true);
  assert.equal(doc.activeElement, id("tools-menu-button"));
});

test("choosing an item closes the menu behind it", () => {
  // Every item here opens a dialog; a menu left hanging open behind a modal is
  // the clunkiest possible outcome.
  const { id, click } = mount();
  click(id("tools-menu-button"));
  click(id("history-toggle"));
  assert.equal(id("tools-menu").hidden, true);
});

test("interacting inside the menu without choosing an item keeps it open", () => {
  // The preset selector lives in a menu section; changing it must not dismiss
  // the menu before the operator can press Launch.
  const { id, click } = mount();
  click(id("app-menu-button"));
  click(id("preset-select"));
  assert.equal(id("app-menu").hidden, false);
});

test("every control moved behind a trigger is still present and keeps its id", () => {
  // The point of the pass was to reduce what is on screen, never to drop a
  // control: each of these already has a handler bound to this exact id.
  const { id } = mount();
  for (const control of [
    "preset-select",
    "launch-preset-button",
    "delete-preset-button",
    "restore-presets-button",
    "refresh-button",
    "preflight-toggle",
    "update-check-button",
    "group-toggle",
    "projects-toggle",
    "prompts-toggle",
    "broadcast-toggle",
    "history-toggle",
    "settings-toggle",
    "explainer-toggle",
  ]) {
    assert.ok(id(control), `${control} disappeared from the chrome`);
  }
});

test("the toolbar keeps only the controls used while scanning the fleet", () => {
  const { doc } = mount();
  const tools = doc.querySelector(".fleet-tools");
  const visible = [...tools.children].filter((child) => !child.classList.contains("menu-wrap"));
  // The control is sometimes the child itself (a bare button) and sometimes
  // wrapped in a label, so look at both rather than only at descendants.
  const ids = visible.map(
    (child) => (child.id || child.querySelector("input, select, button")?.id),
  );
  assert.deepEqual(ids, [
    "filter-input",
    "agent-filter",
    "status-filter",
    "attention-filter",
    "wide-toggle",
  ]);
});

test("the launcher shows what decides a launch and folds the rest away", () => {
  const { doc } = mount();
  const grid = doc.querySelector(".launcher-grid");
  const advanced = grid.querySelector("details.launcher-advanced");
  assert.ok(advanced, "the launcher must group its rarely-set fields");
  assert.equal(advanced.open, false, "advanced options start closed");

  const inside = advanced.querySelectorAll("input, select, textarea, button").length;
  const total = grid.querySelectorAll("input, select, textarea, button").length;
  assert.ok(inside >= 15, `expected the bulk of the fields to be folded away, found ${inside}`);
  assert.ok(total - inside <= 8, `too many controls left at rest: ${total - inside}`);
});

test("folding the launcher hid no field and renamed no id", () => {
  // Every one of these is read by readSpec/writeSpec by id; losing one would
  // silently drop a launch option rather than fail loudly.
  const { id } = mount();
  for (const control of [
    "agent-input", "name-input", "cwd-input", "prompt-input",
    "model-input", "effort-input", "permission-input", "sandbox-input",
    "profile-input", "resume-input", "resume-id-input", "budget-input",
    "search-input", "extra-dirs-input", "template-select", "worktree-input",
    "port-count-input", "port-base-input", "setup-hook-input", "teardown-hook-input",
    "agent-home-input", "env-passthrough-input",
  ]) {
    assert.ok(id(control), `${control} disappeared from the launcher`);
  }
});

test("the advanced summary says what is inside it", () => {
  // A disclosure that only says "Advanced" makes the operator open it to find
  // out whether the thing they want is in there.
  const { doc } = mount();
  const hint = doc.querySelector(".launcher-advanced-hint");
  assert.ok(hint, "the summary must name its contents");
  assert.match(hint.textContent, /model/);
  assert.match(hint.textContent, /sandbox/);
});

test("the panels are disclosures, and do not claim ARIA menu semantics", () => {
  // They used to be `role="menu"` with `role="menuitem"` children. Two things
  // made that a false claim: the app panel holds a <select> and a heading, which
  // are invalid children of a menu and make a screen reader announce the wrong
  // item count; and `wireOverflowMenus` implements no arrow-key movement, which
  // the menu pattern requires once the role is taken. What it does implement —
  // a trigger with aria-expanded/aria-controls, Tab through the contents,
  // Escape and outside-click to close — is exactly a disclosure.
  const { doc, id } = mount();
  for (const panel of ["app-menu", "tools-menu"]) {
    assert.equal(id(panel).getAttribute("role"), null, `${panel} claims a role it does not keep`);
  }
  assert.equal(doc.querySelectorAll('[role="menuitem"]').length, 0);
  assert.equal(doc.querySelectorAll(".menu [role]").length, 0);
  // The half that must survive: the trigger still announces the relationship.
  for (const [button, panel] of [["app-menu-button", "app-menu"], ["tools-menu-button", "tools-menu"]]) {
    assert.equal(id(button).getAttribute("aria-controls"), panel);
    assert.equal(id(button).getAttribute("aria-expanded"), "false");
  }
});
