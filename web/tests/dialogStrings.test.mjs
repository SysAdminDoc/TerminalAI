// The check that would have caught the launcher's untranslated disclosure.
//
// `i18n.test.mjs` asserts the forward direction — every catalogued message is
// referenced, and every `data-i18n` attribute the shell uses is handled by the
// localizer. Both were true while 93 user-facing strings sat in the launcher
// with no attribute at all, because a check that reads the catalog and the
// attributes cannot see text that is in neither.
//
// This reads the shell's own markup instead and asks the opposite question: does
// every text-bearing element inside a dialog get its words from somewhere the
// catalog can reach — an attribute on itself, an attribute on an ancestor whose
// message includes it, or a runtime writer that puts translated text there.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { JSDOM } from "jsdom";

import { appSource } from "./appSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const renderer = appSource();

/// Captions that are deliberately not words.
///
/// The wide row's column headings are three- to five-letter abbreviations of
/// the units beneath them, and they are the same in the row markup and in the
/// dialogs that mirror it. Translating "CTX" produces a longer string in a cell
/// measured to the character; leaving them is a decision, and this is where it
/// is recorded rather than an omission.
const UNTRANSLATED_CAPTIONS = new Set(["MODEL", "EFFORT", "COST", "MEM", "CTX", "TEAM"]);

/// Text belonging to this element rather than to its children.
function ownText(element) {
  return [...element.childNodes]
    .filter((node) => node.nodeType === 3)
    .map((node) => node.textContent.trim())
    .join(" ")
    .trim();
}

function localized(element) {
  for (let node = element; node; node = node.parentElement) {
    if (node.hasAttribute?.("data-i18n")) return true;
  }
  return false;
}

/// Something in the renderer writes this element's text at runtime.
///
/// Read from the source rather than executed, because these are written from
/// event handlers a fixture cannot reach. An id that appears nowhere is the
/// finding; an id that appears is taken as written, which is the same trust the
/// forward check already places in a `data-i18n` attribute.
function writtenAtRuntime(element) {
  // The element's *own* id, never an ancestor's. Walking up finds the dialog,
  // whose id every renderer mentions, and that turned this check into one that
  // passes for anything inside a dialog -- which is every element it looks at.
  // Caught by baiting it: removing a `data-i18n` attribute did not fail it.
  return Boolean(element.id) && renderer.includes(`"${element.id}"`);
}

test("every word inside a dialog comes from the catalog or a runtime writer", () => {
  const dom = new JSDOM(html);
  const offenders = [];
  for (const dialog of dom.window.document.querySelectorAll("dialog")) {
    for (const element of dialog.querySelectorAll("*")) {
      const text = ownText(element);
      if (!text || UNTRANSLATED_CAPTIONS.has(text)) continue;
      if (localized(element) || writtenAtRuntime(element)) continue;
      offenders.push(`${dialog.id} → <${element.tagName.toLowerCase()}> ${JSON.stringify(text)}`);
    }
  }
  assert.deepEqual(offenders, [], "dialog text with no catalog message behind it");
});

test("every placeholder inside a dialog is catalogued", () => {
  // Placeholders are the half a reader forgets: they are user-facing text that
  // never appears in the markup as a text node, so the check above cannot see
  // them at all.
  const dom = new JSDOM(html);
  const offenders = [];
  for (const dialog of dom.window.document.querySelectorAll("dialog")) {
    for (const element of dialog.querySelectorAll("[placeholder]")) {
      if (element.hasAttribute("data-i18n-placeholder")) continue;
      offenders.push(`${dialog.id} → #${element.id} ${JSON.stringify(element.placeholder)}`);
    }
  }
  assert.deepEqual(offenders, [], "dialog placeholders with no catalog message behind them");
});

test("a message that wraps its own markup would lose it, so none does", () => {
  // `localizeDom` assigns `textContent`, which replaces every child. A message
  // like "Known projects <em>from your registered roots</em>" therefore rendered
  // as one flat run of body text and quietly lost the styling the stylesheet
  // targets at `.field > span em`. Both such messages were split in two.
  const dom = new JSDOM(html);
  const offenders = [];
  for (const element of dom.window.document.querySelectorAll("[data-i18n]")) {
    if (element.children.length > 0) {
      offenders.push(`${element.dataset.i18n} wraps <${element.children[0].tagName.toLowerCase()}>`);
    }
  }
  assert.deepEqual(offenders, [], "data-i18n on an element with children flattens them");
});
