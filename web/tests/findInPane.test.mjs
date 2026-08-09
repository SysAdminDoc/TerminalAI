import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

import { renderSearchResults, searchSummary } from "../src/fleetSearch.js";
import { moduleSource } from "./appSource.mjs";
import { cssSource } from "./cssSource.mjs";

/// The same escaper `main.js` binds.
const escapeHtml = (value) =>
  String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");

const terminalPane = moduleSource("terminalPane.js");
const terminalHistory = moduleSource("terminalHistory.js");
const workspacePages = moduleSource("workspacePages.js");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const css = cssSource();
const catalog = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

test("the search addon is a pinned dependency and is actually loaded", () => {
  // The pane had no way to find anything in it. `addon-search` 0.16.0 carries
  // the SearchLineCache that makes a long buffer searchable at all.
  assert.equal(pkg.dependencies["@xterm/addon-search"], "0.16.0");
  assert.match(terminalPane, /SearchAddon/);
  assert.match(terminalPane, /state\.searchAddon = new SearchAddon\(\)/);
  assert.match(terminalPane, /state\.terminal\.loadAddon\(state\.searchAddon\)/);
});

test("the match count comes from the addon rather than being counted twice", () => {
  // The addon knows what it found. Recounting in the renderer would be a
  // second implementation of the search that can disagree with the highlights.
  assert.match(terminalPane, /state\.searchAddon\.onDidChangeResults\(\(results\) => renderFindCount\(results\)\)/);
});

test("a scan in progress is not reported as zero matches", () => {
  // `resultCount` is -1 while the addon is still walking a long buffer. Showing
  // "no matches" then is a wrong answer that arrives before the right one and
  // looks identical to it.
  const fn = terminalHistory.slice(terminalHistory.indexOf("function renderFindCount"));
  const body = fn.slice(0, fn.indexOf("\n/// Prepend output"));
  assert.match(body, /results\.resultCount < 0/);
  assert.match(body, /t\("find-searching"\)/);
  assert.match(body, /results\.resultCount === 0/);
  assert.match(body, /t\("find-none"\)/);
  assert.match(body, /t\("find-position"/);
  for (const key of ["find-searching", "find-none", "find-position"]) {
    assert.ok(catalog.includes(`${key} =`), `${key} is missing from the catalog`);
  }
});

test("closing the find bar clears the decorations it left behind", () => {
  // xterm keeps highlight decorations until told otherwise, so a hidden bar
  // with a live search leaves the pane marked up for a query the operator can
  // no longer see or change.
  const fn = terminalHistory.slice(terminalHistory.indexOf("function toggleFind"));
  const body = fn.slice(0, fn.indexOf("\n/// Run the current query"));
  assert.match(body, /state\.searchAddon\?\.clearDecorations\(\)/);
  assert.match(body, /\$\("terminal-find-count"\)\.textContent = ""/);
  // An emptied field is the same situation: nothing is being searched.
  const run = terminalHistory.slice(terminalHistory.indexOf("function runFind"));
  assert.match(run.slice(0, run.indexOf("\n/// Report the addon")), /if \(!needle\) \{/);
});

test("the highlight colours are theme tokens, not a second palette", () => {
  // Decorations are painted into the same canvas no DOM contrast gate can see.
  // Literals here would be the hardcoded-terminal-theme defect again, in the
  // one place the guard against it was not looking.
  const run = terminalHistory.slice(terminalHistory.indexOf("function runFind"));
  const body = run.slice(0, run.indexOf("\n/// Report the addon"));
  assert.doesNotMatch(body, /#[0-9a-f]{6}/i, "no literal colours in the decorations");
  assert.match(body, /matchOverviewRuler: token\("--yellow"\)/);
  assert.match(body, /matchBackground: token\("--term-selection"\)/);
});

test("a fleet search excerpt cannot inject markup", () => {
  // The excerpt is literally arbitrary agent output — whatever a tool printed,
  // including a file it read. This is the widest untrusted-content path the
  // search adds, so the assertion is against real markup: grepping the source
  // for `escapeHtml` passes whether or not the value reaching the DOM was ever
  // escaped, which is the proxy this repo has already been bitten by.
  const hostile = '<img src=x onerror="alert(1)">';
  const markup = renderSearchResults(
    [
      {
        id: `s0001" onclick="alert(1)`,
        name: hostile,
        total_matches: 1,
        truncated: false,
        hits: [{ line: 7, text: hostile, matches: 1 }],
      },
    ],
    { escape: escapeHtml, translate: (key) => key, needle: hostile },
  );
  const dom = new JSDOM(`<body>${markup}</body>`);
  assert.equal(dom.window.document.querySelectorAll("img").length, 0, "no element was created");
  assert.equal(
    dom.window.document.querySelector("code").textContent,
    hostile,
    "the excerpt survives as text, exactly as printed",
  );
  assert.equal(
    dom.window.document.querySelector("[data-search-focus]").dataset.searchFocus,
    `s0001" onclick="alert(1)`,
    "a quote in the id closes no attribute",
  );
  // The characters `onerror=` do survive — as text inside an escaped excerpt,
  // which is correct: the operator searched for output and that is the output.
  // Inertness is what the DOM assertions above establish, and it is the only
  // thing that matters.
  const button = dom.window.document.querySelector("[data-search-focus]");
  assert.equal(button.getAttribute("onclick"), null, "no handler attribute was created");
});

test("an empty result says nothing matched rather than rendering nothing", () => {
  // A blank panel reads as "the search did not run".
  const markup = renderSearchResults([], {
    escape: escapeHtml,
    translate: (key) => key,
    needle: "e0432",
  });
  assert.match(markup, /fleet-search-none/);
});

test("the summary counts sessions and occurrences separately", () => {
  // Two different numbers: how many places to look, and how much is there.
  const summary = searchSummary([
    { total_matches: 3 },
    { total_matches: 7 },
  ]);
  assert.deepEqual(summary, { sessions: 2, total: 10 });
  assert.deepEqual(searchSummary([]), { sessions: 0, total: 0 });
});

test("a needle too short to be worth a fleet read is refused before the call", () => {
  // A one-character search matches most of every transcript and costs a read
  // of the whole fleet's disk tier to establish that. The core refuses it too;
  // this stops the round trip happening at all.
  const fn = workspacePages.slice(workspacePages.indexOf("async function runFleetSearch"));
  const body = fn.slice(0, fn.indexOf("\n/// One session's matches"));
  const guard = body.indexOf("needle.length < 2");
  const call = body.indexOf('invoke("search_fleet"');
  assert.ok(guard > 0 && call > guard, "the length check must precede the invoke");
  assert.match(body, /t\("fleet-search-too-short"\)/);
});

test("a capped result says so rather than reading as complete", () => {
  // The per-session count stays exact after the excerpts stop. Without the
  // note, two excerpts under a count of two hundred looks like a bug in the
  // count rather than a deliberate cap on the excerpts.
  const session = (truncated) => ({
    id: "s0001",
    name: "row",
    total_matches: 200,
    truncated,
    hits: [{ line: 1, text: "error", matches: 1 }],
  });
  const options = { escape: escapeHtml, translate: (key) => key, needle: "error" };
  assert.match(renderSearchResults([session(true)], options), /search-truncated/);
  assert.doesNotMatch(renderSearchResults([session(false)], options), /search-truncated/);
  assert.ok(catalog.includes("fleet-search-truncated ="));
  assert.ok(catalog.includes("fleet-search-hits ="));
});

test("the find bar is present, hidden by default, and announces its count", () => {
  assert.match(html, /id="terminal-find"[^>]*hidden/);
  assert.match(html, /id="terminal-find-toggle"[^>]*aria-pressed="false"/);
  assert.match(html, /id="terminal-find-count"[^>]*role="status"[^>]*aria-live="polite"/);
  // A strip, not an overlay: an overlay covers the output being searched.
  assert.match(css, /\.terminal-find \{/);
  assert.match(css, /\.terminal-find\[hidden\] \{\s*display: none;/);
});
