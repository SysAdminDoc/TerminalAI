import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

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

test("the review view owns its scroll surface", () => {
  // Reading the CSSOM catches the exact failure mode where a literal escape
  // turns the selector into `n .review-view` while the source still looks
  // plausibly close to correct.
  const dom = new JSDOM(`<style>${css}</style>`);
  const rules = [...dom.window.document.styleSheets[0].cssRules].filter(
    (rule) => rule.selectorText === ".review-view",
  );
  assert.equal(rules.length, 1, "the stylesheet must contain one real .review-view rule");
  assert.equal(rules[0].style.height, "calc(100% - 54px)");
  assert.equal(rules[0].style.overflow, "auto");
  assert.equal(rules[0].style.padding, "12px 18px 18px");
});

test("light mode keeps interactive surfaces readable", () => {
  const lightStart = css.indexOf("@media (prefers-color-scheme: light)");
  const lightEnd = css.indexOf("\n}", lightStart);
  assert.ok(lightStart >= 0 && lightEnd > lightStart, "light theme block must be closed");
  const lightCss = css.slice(lightStart, lightEnd);
  const afterLight = css.slice(lightEnd + 2);
  assert.doesNotMatch(
    afterLight,
    /background(?:-color)?\s*:\s*(?:rgba?\(|#[0-9a-f])/i,
    "rules after the light theme must not reintroduce hardcoded backgrounds",
  );
  assert.match(lightCss, /\.fleet-row:hover, \.fleet-row:focus-visible \{[^}]*background: var\(--surface0\);/);
  assert.match(lightCss, /\.external-view \{[^}]*background: var\(--mantle\);/);
  assert.match(lightCss, /\.pinned-pane \{[^}]*background: var\(--mantle\);/);
  assert.match(lightCss, /\.filter-select select \{[^}]*background: var\(--mantle\);/);

  const tokens = Object.fromEntries(
    [...lightCss.matchAll(/--([\w]+):\s*(#[0-9a-f]{6})/g)].map(([, name, value]) => [name, value]),
  );
  const luminance = (hex) => {
    const channels = [1, 3, 5].map((offset) => parseInt(hex.slice(offset, offset + 2), 16) / 255);
    return channels
      .map((channel) => (channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4))
      .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
  };
  const contrast = (foreground, background) => {
    const foregroundLuminance = luminance(foreground);
    const backgroundLuminance = luminance(background);
    return (
      (Math.max(foregroundLuminance, backgroundLuminance) + 0.05) /
      (Math.min(foregroundLuminance, backgroundLuminance) + 0.05)
    );
  };
  for (const foreground of [tokens.text, tokens.subtext0, tokens.subtext1]) {
    for (const background of [tokens.mantle, tokens.surface0]) {
      assert.ok(
        contrast(foreground, background) >= 4.5,
        `${foreground} on ${background} is below 4.5:1`,
      );
    }
  }
});

test("screen reader mode is explicit and opt-in", () => {
  assert.match(html, /id="screen-reader-toggle"[^>]*aria-pressed="false"/);
  assert.match(html, /id="screen-reader-toggle"[^>]*aria-label="Toggle screen reader mode"/);
  assert.match(catalog, /screen-reader-enable = Enable screen reader mode \(disables right-click copy and paste\)/);
  assert.match(main, /screenReaderMode: false,/);
  assert.match(main, /state\.terminal\.options\.screenReaderMode = state\.screenReaderMode/);
  assert.match(main, /screen-reader-toggle.*setScreenReaderMode\(!state\.screenReaderMode\)/);
});
