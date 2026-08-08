import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";
import { appSource } from "./appSource.mjs";
import { cssSource } from "./cssSource.mjs";

const css = cssSource();
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = appSource();
const catalog = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("small UI text does not use decorative contrast tokens", () => {
  assert.doesNotMatch(css, /(?:^|[;{]\s*)color:\s*var\(--(?:overlay0|overlay1|surface1|surface2)\)/m);
  assert.match(css, /\.fleet-row\.row-focused[^}]*background: var\(--surface0\);/);
  assert.match(css, /--subtext0: #43465c;/);
  assert.match(css, /--subtext1: #4c4f69;/);
});

test("forced colors preserves the xterm surface and maps controls to system colors", () => {
  assert.match(css, /@media \(forced-colors: active\)/);
  assert.match(css, /\.terminal-host \.xterm \{\s*forced-color-adjust: none;\s*\}/);
  assert.match(css, /background: Canvas;/);
  assert.match(css, /color: CanvasText;/);
  assert.match(css, /background: Highlight;/);
  assert.equal((css.match(/forced-color-adjust/g) ?? []).length, 1);
});

test("reduced motion disables the remaining spinner and glow effects", () => {
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(css, /\.fleet-loading::before \{\s*animation: none !important;\s*\}/);
  assert.match(
    css,
    /\.unread-dot,\s*\.pulse-peach,\s*\.pulse-yellow,\s*\.pulse-mauve,\s*\.pulse-red \{\s*box-shadow: none !important;\s*\}/,
  );
});

test("status metadata has matching tone and pulse classes", () => {
  const statusMeta = main.slice(main.indexOf("const STATUS_META"), main.indexOf("const STATUS_KEYS"));
  const preflightMeta = main.slice(main.indexOf("const PREFLIGHT_META"), main.indexOf("const RELEASES_ENDPOINT"));
  const statusTones = [...statusMeta.matchAll(/tone: "([^"]+)"/g)].map(([, tone]) => tone);
  const preflightTones = [...preflightMeta.matchAll(/tone: "([^"]+)"/g)].map(([, tone]) => tone);

  for (const tone of new Set([...statusTones, ...preflightTones])) {
    assert.match(css, new RegExp(`\\.tone-${tone}\\s*\\{`), `${tone} has no tone class`);
  }
  for (const tone of new Set(statusTones)) {
    assert.match(css, new RegExp(`\\.pulse-${tone}\\s*\\{`), `${tone} has no pulse class`);
  }
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

test("dialog content shares the action footer inset", () => {
  const dom = new JSDOM(`<style>${css}</style>`);
  const rules = [...dom.window.document.styleSheets[0].cssRules];
  const paddingRule = rules.find((rule) => rule.selectorText?.includes(".launcher-dialog > .dialog-head"));
  assert.ok(paddingRule, "shared dialog padding rule must exist");
  for (const selector of [
    ".launcher-dialog > .dialog-head",
    ".launcher-dialog > .rollup-body",
    ".launcher-dialog > .broadcast-body",
    ".launcher-dialog > .explainer-body",
    ".launcher-dialog > .work-run-bar",
    ".launcher-dialog > .work-run-body",
  ]) {
    assert.match(paddingRule.selectorText, new RegExp(selector.replaceAll(".", "\\.")));
  }
  assert.equal(paddingRule.style.padding, "20px 26px");

  const actionRule = rules.find((rule) => rule.selectorText === ".dialog-actions");
  assert.ok(actionRule, "dialog action rule must exist");
  assert.equal(actionRule.style.padding, "15px 26px 19px");
});

test("the preset selector keeps a visible keyboard focus indicator", () => {
  // The selector moved into the overflow menu, but it is still reachable by
  // keyboard, and a bare <select> there would lose the ring the rest of the app
  // has. The assertion follows the control; the guarantee does not change.
  assert.match(html, /class="menu-section"[^>]*>[\s\S]*?id="preset-select"/);
  const dom = new JSDOM(`<style>${css}</style>`);
  const rule = [...dom.window.document.styleSheets[0].cssRules].find(
    (candidate) => candidate.selectorText === ".menu-section select:focus-visible",
  );
  assert.ok(rule, "the menu's select must style its focused state");
  assert.equal(rule.style.borderColor, "var(--mauve)");
  assert.equal(rule.style.outline, "2px solid var(--blue)");
  assert.match(css, /--mauve: #cba6f7;/);
  assert.match(css, /--mauve: #6f2dbd;/);
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

test("the terminal follows the theme instead of carrying its own palette", () => {
  // The pane fills most of the window and its canvas is not DOM text, so no
  // contrast gate can see what it painted. It used to hold a literal dark
  // Catppuccin theme regardless of `prefers-color-scheme`: in light mode a light
  // panel framed a hard dark rectangle, and focusing a session flipped the
  // pane's apparent theme.
  assert.match(main, /theme: terminalTheme\(\)/);
  assert.doesNotMatch(
    main,
    /background:\s*"#[0-9a-f]{6}"/i,
    "the renderer must not reintroduce a hardcoded terminal background",
  );
  // The pane's own surface is the terminal's, so the two cannot drift apart.
  assert.match(css, /\.terminal-panel \{[^}]*background: var\(--term-bg\);/);
  // A window whose OS theme changes must repaint; the canvas is painted from
  // values read once, unlike the CSS around it.
  assert.match(main, /prefers-color-scheme: dark[\s\S]*?options\.theme = terminalTheme\(\)/);

  const palette = (block) =>
    Object.fromEntries(
      [...block.matchAll(/--(term-[\w-]+|red|green|yellow|blue|mauve|teal):\s*(#[0-9a-f]{6})/g)]
        .map(([, name, value]) => [name, value]),
    );
  const luminance = (hex) => {
    const channels = [1, 3, 5].map((offset) => parseInt(hex.slice(offset, offset + 2), 16) / 255);
    return channels
      .map((channel) => (channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4))
      .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
  };
  const contrast = (foreground, background) => {
    const [high, low] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
    return (high + 0.05) / (low + 0.05);
  };

  const lightStart = css.indexOf("@media (prefers-color-scheme: light)");
  assert.ok(lightStart > 0);
  for (const [mode, block] of [
    ["dark", css.slice(0, lightStart)],
    ["light", css.slice(lightStart)],
  ]) {
    const tokens = palette(block);
    assert.ok(tokens["term-bg"], `${mode} declares no terminal background`);
    // ANSI black is excluded on purpose: it is the colour programs print when
    // they mean "the background", and every terminal renders it near-invisible
    // against a dark surface. Everything an agent actually prints in must read.
    for (const name of ["term-fg", "term-cursor", "term-white", "red", "green", "yellow", "blue", "mauve", "teal"]) {
      const value = tokens[name];
      assert.ok(value, `${mode} declares no --${name}`);
      assert.ok(
        contrast(value, tokens["term-bg"]) >= 4.5,
        `${mode} --${name} (${value}) is ${contrast(value, tokens["term-bg"]).toFixed(2)}:1 on the terminal background`,
      );
    }
  }
});
