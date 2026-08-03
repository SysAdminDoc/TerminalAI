import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const catalog = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("small UI text does not use decorative contrast tokens", () => {
  assert.doesNotMatch(css, /(?:^|[;{]\s*)color:\s*var\(--(?:overlay0|overlay1|surface1|surface2)\)/m);
  assert.match(css, /\.fleet-row\.row-focused[^}]*background: var\(--surface0\);/);
  assert.match(css, /--subtext0: #43465c;/);
  assert.match(css, /--subtext1: #4c4f69;/);
});

test("forced colors preserves the xterm surface and maps controls to system colors", () => {
  assert.match(css, /@media \(forced-colors: active\)/);
  assert.match(css, /\.terminal-host \.xterm \{ forced-color-adjust: none; \}/);
  assert.match(css, /background: Canvas;/);
  assert.match(css, /color: CanvasText;/);
  assert.match(css, /background: Highlight;/);
  assert.equal((css.match(/forced-color-adjust/g) ?? []).length, 1);
});

test("reduced motion disables the remaining spinner and glow effects", () => {
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(css, /\.fleet-loading::before \{ animation: none !important; \}/);
  assert.match(css, /\.unread-dot, \.pulse-peach, \.pulse-yellow, \.pulse-mauve \{ box-shadow: none !important; \}/);
});

test("screen reader mode is explicit and opt-in", () => {
  assert.match(html, /id="screen-reader-toggle"[^>]*aria-pressed="false"/);
  assert.match(html, /id="screen-reader-toggle"[^>]*aria-label="Toggle screen reader mode"/);
  assert.match(catalog, /screen-reader-enable = Enable screen reader mode \(disables right-click copy and paste\)/);
  assert.match(main, /screenReaderMode: false,/);
  assert.match(main, /state\.terminal\.options\.screenReaderMode = state\.screenReaderMode/);
  assert.match(main, /screen-reader-toggle.*setScreenReaderMode\(!state\.screenReaderMode\)/);
});
