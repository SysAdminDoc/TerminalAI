import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { renderFixtureRow } from "./rowFixture.mjs";
import { appSource } from "./appSource.mjs";

// The fleet row markup lives in `rowMarkup.js` since it was extracted out of
// this file. These assertions are about what the app renders, not about which
// module holds the template, so they read both.
const main =
  appSource() +
  readFileSync(new URL("../src/rowMarkup.js", import.meta.url), "utf8");
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
  // Rendered, not sliced out of the source: the markup is built from
  // concatenated pieces, so a source slice proves nothing about what an
  // operator is actually handed.
  const row = renderFixtureRow({ status: "needs-you" });
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
  for (const key of ["empty-launch-first", "empty-first-root", "empty-first-explainer"]) {
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

test("a question the agent will answer for itself shows how long is left", () => {
  // The grace period used to spend thirty of the operator's sixty seconds, and
  // nothing on the row said the clock was running at all.
  assert.match(main, /const AGENT_AUTO_RESOLVE_SECONDS = 60;/);
  assert.match(main, /function answerSecondsRemaining\(/);
  assert.match(main, /function answerCountdownLabel\(/);
  assert.match(main, /class="row-answer-deadline"/);

  // Only the states the agent resolves on its own count down. A permission
  // request waits indefinitely, so a countdown there would be a fiction.
  const start = main.indexOf("function expiresWithoutAnAnswer(");
  const body = main.slice(start, main.indexOf("\n}", start));
  assert.match(body, /"awaiting-input", "needs-you"/);
  assert.doesNotMatch(body, /needs-approval/);

  const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
  for (const key of ["answer-deadline", "answer-deadline-passed", "answer-deadline-explained"]) {
    assert.ok(new RegExp(`^${key} =`, "m").test(ftl), `${key} missing from terminalai.ftl`);
  }
  assert.match(ftl, /^answer-deadline = .*\{ \$seconds \}/m);
});
