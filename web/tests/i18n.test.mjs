import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FluentBundle, FluentResource } from "@fluent/bundle";

const catalogSource = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const i18n = readFileSync(new URL("../src/i18n.js", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");

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

test("compact rows keep localized status words out of the fixed-height line", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  assert.match(css, /\.fleet-row \{[^}]*min-height: 28px/);
  assert.match(css, /\.row-folder \.row-status-label[^}]*display: none/);
  assert.match(main, /title="\$\{escapeHtml\(label\)\}"/);
});

