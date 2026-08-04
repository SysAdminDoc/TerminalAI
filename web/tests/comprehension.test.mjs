import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("every icon-only button says what it does", () => {
  // A button reading "Close" needs no tooltip; one reading "◇" is a guess, and
  // the guess is often "delete". This is the rule worth enforcing — demanding a
  // title on a button that already has words would be noise that later edits
  // work around rather than satisfy.
  const iconButtons = [...html.matchAll(/(<button\b[^>]*>)([\s\S]*?)<\/button>/g)].filter(
    ([, , inner]) => {
      const text = inner.replace(/<[^>]+>/g, "").trim();
      return text.length <= 3 && !/[A-Za-z]{2}/.test(text);
    },
  );
  assert.ok(iconButtons.length >= 8, `only ${iconButtons.length} icon buttons found`);
  const unlabelled = iconButtons
    .filter(([, opening]) => !/title=|aria-label=/.test(opening))
    .map(([, opening]) => opening);
  assert.deepEqual(unlabelled, [], "icon-only buttons with no tooltip");
});

test("controls built at runtime carry a tooltip too", () => {
  // Row actions never appear in index.html, so the static sweep above cannot
  // see them — and they are the densest icons in the app.
  const row = main.slice(main.indexOf('<div class="row-actions">'), main.indexOf("function updateSession"));
  const buttons = [...row.matchAll(/<button[^>]*data-action="([a-z]+)"[^>]*/g)];
  assert.ok(buttons.length >= 5, `only ${buttons.length} row actions found`);
  for (const [markup, action] of buttons) {
    assert.match(markup, /title=/, `row action ${action} has no tooltip`);
    assert.match(markup, /aria-label=/, `row action ${action} has no accessible name`);
  }
});

test("the row model is explained in the app, not only in the README", () => {
  // The one thing that makes this tool different is also the thing nobody
  // guesses: rows are not terminals, and only the focused session has one.
  assert.match(html, /id="explainer-dialog"/);
  assert.match(html, /id="explainer-toggle"/);
  assert.match(main, /\$\("explainer-toggle"\)\.addEventListener/);
  for (const key of ["explainer-title", "explainer-rows", "explainer-focus", "explainer-attention"]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} missing`);
  }
});

test("first-run guidance points at the first thing to do", () => {
  // The empty state says the fleet is empty; it must also say what to do, and
  // registering a root is the step that makes everything else easy.
  assert.match(html, /id="empty-state"/);
  for (const key of ["empty-first-launch", "empty-first-root", "empty-first-explainer"]) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} missing`);
  }
});

test("every status a row can show has a plain-language description", () => {
  // The short label fits the row; the description is what the tooltip and the
  // explainer use, and it is where "thinking" stops being jargon.
  const meta = main.slice(main.indexOf("const STATUS_META"), main.indexOf("const STATUS_KEYS"));
  const shortKeys = [...meta.matchAll(/short: "([a-z0-9-]+)"/g)].map((match) => match[1]);
  assert.ok(shortKeys.length >= 8, `only ${shortKeys.length} statuses found`);
  for (const key of shortKeys) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} has no label`);
    assert.ok(
      new RegExp(`^${key}-explained = `, "m").test(ftl),
      `${key} has no plain-language description`,
    );
  }
});

test("the dwell time says what it is measuring", () => {
  // A bare "4m" beside a status is ambiguous — since when?
  assert.ok(/^dwell-explained = /m.test(ftl));
  assert.match(main, /t\("dwell-explained"/);
});

test("no message key is defined twice", () => {
  // The Rust side loads the same catalog and treats an override as an error,
  // so a duplicate here fails a test far away from the edit that caused it.
  const keys = [...ftl.matchAll(/^([a-z][a-z0-9-]*) = /gm)].map((match) => match[1]);
  const seen = new Set();
  const duplicates = keys.filter((key) => !seen.add(key));
  assert.deepEqual(duplicates, [], "duplicate message keys");
});
