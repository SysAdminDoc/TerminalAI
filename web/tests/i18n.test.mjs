import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

import { FluentBundle, FluentResource } from "@fluent/bundle";

const catalogSource = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const i18n = readFileSync(new URL("../src/i18n.js", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
// Every module, read from the directory rather than a hand-kept list: a list
// silently drops a newly extracted renderer out of coverage, which is the
// opposite of what this file is for.
const rendererSources = readdirSync(new URL("../src/", import.meta.url))
  .filter((name) => name.endsWith(".js"))
  .map((name) => readFileSync(new URL(`../src/${name}`, import.meta.url), "utf8"))
  .join("\n");

function bundle() {
  const instance = new FluentBundle(["en-US"], { useIsolating: false });
  instance.addResource(new FluentResource(catalogSource));
  return instance;
}

function format(instance, id, args = {}) {
  const message = instance.getMessage(id);
  assert.ok(message?.value, `missing catalog message: ${id}`);
  return instance.formatPattern(message.value, args, []);
}

test("Rust and JS share one parseable Fluent catalog", () => {
  const instance = bundle();
  assert.equal(format(instance, "status-working"), "Working");
  assert.equal(format(instance, "sessions-count", { count: 2 }), "2 sessions");
  assert.match(i18n, /terminalai\.ftl\?raw/);
  assert.match(main, /from "\.\/i18n\.js"/);
});

test("the renderer delegates plural and dwell formatting to Intl", () => {
  assert.match(i18n, /new Intl\.PluralRules/);
  assert.match(i18n, /new Intl\.RelativeTimeFormat/);
  assert.match(main, /countMessage\("count-session"/);
  assert.match(main, /relativeDwell/);
});

test("every shell localization attribute is handled by the DOM localizer", () => {
  const attributes = new Set(
    [...html.matchAll(/\s(data-i18n(?:-[a-z-]+)?)=/g)].map((match) => match[1]),
  );
  for (const attribute of attributes) {
    assert.ok(
      i18n.includes('querySelectorAll("[' + attribute + ']")'),
      attribute + " is not handled by localizeDom",
    );
  }
});

test("every non-explained catalog message has a renderer reference", () => {
  const source = rendererSources + "\n" + html;
  const keys = [...catalogSource.matchAll(/^([a-z][a-z0-9-]*) = /gm)].map((match) => match[1]);
  const dynamicFamilies = [
    ["reason-", "reason-${"],
    ["source-", "source-${"],
    ["group-", "group-${"],
    ["queue-pause-", "queue-pause-${"],
  ];
  const referenced = (key) => {
    if (source.includes(key)) return true;
    if (key.endsWith("-explained")) return true;
    if (key.endsWith("-one") || key.endsWith("-other")) {
      const prefix = key.replace(/-(one|other)$/, "");
      return main.includes('countMessage("' + prefix + '"');
    }
    return dynamicFamilies.some(
      ([prefix, marker]) => key.startsWith(prefix) && source.includes(marker),
    );
  };
  const orphaned = keys.filter((key) => !referenced(key));
  assert.deepEqual(orphaned, [], "catalog messages without a renderer reference");
});

test("compact rows keep localized status words out of the fixed-height line", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  assert.match(css, /\.fleet-row \{[^}]*min-height: 28px/);
  assert.match(css, /\.row-folder \.row-status-label[^}]*display: none/);
  // The status word lives in a tooltip on the compact row, carrying the reason
  // when the phase has one.
  assert.match(main, /title="\$\{escapeHtml\(detail \|\| label\)\}"/);
});
