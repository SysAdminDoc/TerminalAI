import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";
import { cssSource } from "./cssSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = appSource();
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const styles = cssSource();

test("the Wide control changes its tooltip with the action it will take", () => {
  const render = main.slice(main.indexOf("function renderRows"), main.indexOf("\nfunction renderSnapshotLoading"));
  assert.match(render, /const wideTitle = state\.wideMode \? "button-hide-model-effort-cost" : "button-show-model-effort-cost";/);
  assert.match(render, /wideToggle\.setAttribute\("data-i18n-title", wideTitle\);/);
  assert.match(ftl, /^button-hide-model-effort-cost = Hide model, effort, and cost$/m);
  assert.match(html, /id="wide-toggle"[^>]*data-i18n-title="button-show-model-effort-cost"/);
});

test("the grouping tooltip says that the button cycles modes", () => {
  assert.match(ftl, /^button-group-list = Cycle grouping:/m);
  assert.match(html, /id="group-toggle"[^>]*data-i18n-title="button-group-list"/);
});

test("the umbrella attention label is distinct from the Needs you status", () => {
  assert.match(main, /countMessage\("count-needs-attention", needsAttention\)/);
  assert.match(ftl, /^count-needs-attention-one = /m);
  assert.match(ftl, /^filter-status-attention = Needs attention$/m);
  assert.doesNotMatch(ftl, /^filter-status-attention = Needs you$/m);
  assert.match(html, /<option value="attention"[^>]*>Needs attention<\/option>/);
});

test("neutral toasts use a neutral border and errors keep the error border", () => {
  const base = styles.match(/\.toast \{([^}]*)\}/)?.[1] ?? "";
  assert.match(base, /border: 1px solid rgba\(88, 91, 112, \.45\)/);
  assert.doesNotMatch(base, /243, 139, 168/);
  assert.match(styles, /\.toast-error \{\s*border-color: rgba\(243, 139, 168, \.4\);\s*\}/);
});
