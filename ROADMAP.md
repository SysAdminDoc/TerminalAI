# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; completed items are removed.

## Verification Gaps — 2026-08-08

- [ ] P2 — The WebdriverIO end-to-end gate reaches the app, then stops on the preflight view
  Category: verification
  Where: `web/tests/ui.e2e.mjs`, `web/src/main.js` (`loadPreflight`, `setPreflightMode`)
  Problem: `npm --prefix web run test:e2e` now loads the real frontend — the loading state appears, the mocked fleet reaches the renderer (`#fleet-summary` reads `0/3 live`) — and then fails because `#fleet-list` stays hidden behind `#preflight-view`. The spec mocks `preflight_report` with every check `ok`, so something is putting the window into preflight mode anyway, and the fleet list is never shown. This is the only harness that drives the real WebView2 window; until it passes, dialog behaviour is verified in jsdom and in the Chromium audit but never in the shipping shell.
  Evidence: probed inside the running window on 2026-08-08 — `#fleet-loading` false, `#preflight-view` true, `#fleet-summary` "0/3 live", `fleetMock.calls.length` 0 (the wdio mock object does not appear to count calls, so that number is not evidence either way). Fixed on the way here: the harness built the app *without* `custom-protocol`, so `dev = !custom_protocol` in `tauri`'s build script made every run a dev shell pointed at `devUrl` with nothing serving it — the window came up empty and every assertion failed. The wdio plugin's non-`execute` commands were also denied by the ACL on every poll.
  Fix: find what sets `state.preflightMode` when every check is `ok` — the daemon client is deliberately absent under `cfg(feature = "wdio")`, which is the first suspect — and make the spec assert the fleet is visible rather than only that a row exists. `web/tests/ui.e2e.mjs` already carries an unrun scenario for the repeating work run that will exercise the schedule controls once the fleet renders.
  Acceptance: `npm --prefix web run test:e2e` passes on a clean tree and its screenshots show the real fleet, not the first-run check.
  Confidence: Verified (observed twice, and on a stashed tree before any of this session's changes)
  Effort: M

## Audit Findings — 2026-08-07

Fourth audit pass, against `cc53821` / v0.15.0 with a green baseline of 585 Rust (0 failed), 305
frontend (0 failed), clippy clean, and the 13-surface chrome gate clean in both themes at both
widths. No pre-existing test, lint or build failure exists. Verification: every code-path finding
below was traced to a real caller; the theming finding was observed live (Vite + headless Chromium,
both `prefers-color-scheme` values) rather than inferred from source.

- [ ] P3 — Decompose `web/src/main.js`
  Category: maintainability
  Where: `web/src/main.js` (3,685 lines)
  Problem: main.js is still the tree's god file and its highest-churn one. Thirteen modules have now been split out (rowMarkup, terminalPane, updateCheck, workRunPanel, workSchedule, rateLimit, rollup, menus, fleetRows, …), which proves the seams work; what remains fuses the launcher dialog, the fleet renderer, diagnostics, preflight, review, the queue panel and the event loop in one scope where every edit risks every feature.
  Evidence: `wc -l` = 3,685, down from 3,940. The extraction pattern and its per-module tests exist and are green, and `tests/appSource.mjs` already reads every module so a move does not silently drop assertions.
  Fix: continue one seam at a time with the moved-code-unchanged discipline. The next two, in order of size and cohesion: (1) the launcher dialog — `defaultSpec`/`readSpec`/`writeSpec`/`syncAgentFields`/`renderCapabilityFields`/`updatePreview`/`launchCurrentSpec` plus the preset and template loaders, roughly 560 lines; (2) the per-session prompt queue panel — `queueGlyph` through `addQueuedPrompt`, roughly 150. Note that an extracted module must be within 120 columns while `main.js` is only held to a ratchet, so a move drags its long HTML templates into the limit — split them by concatenation, which `workRunPanel.js` now shows the shape of.
  Acceptance: main.js under ~2,500 lines with those two modules extracted, both suites passing without assertion edits, and the chrome gate clean.
  Confidence: Verified
  Effort: L

## Audit Findings — 2026-08-03

Read-only audit pass. Baseline before any of this was found: `cargo test` **411 passed / 0 failed**,
`npm --prefix web test` **195 passed / 0 failed**, `cargo clippy --workspace --all-targets` clean,
`cargo deny --target x86_64-pc-windows-msvc check advisories` ok. **No pre-existing test, lint or
build failure exists** — every item below is invisible to the current suites, which is itself the
theme of the top two findings.

Verification method: the frontend was served with `npx vite --port 5199` and driven headlessly
(Chromium via `playwright-core`, both `prefers-color-scheme` values, 1440px and 1100px), so the
UI findings are observed rather than inferred. Contrast ratios were computed from composited
`getComputedStyle` values in that engine.






## Research-Driven Additions

Filed 2026-08-04 from an external research pass (see `RESEARCH.md`). These are gaps the same-day
code audit above did not reach: platform surfaces the project declines to use, data it collects and
discards, and capabilities the competitive field has made table stakes. Nothing here duplicates an
item above; where the two touch, the note says so.

### P0

### P1

### P2

## Research-Driven Additions — 2026-08-04

Second external research pass (see `RESEARCH.md`), covering ground the first did not: process-
supervision and admission-control theory, Windows distribution and supply chain, VT/terminal
correctness, and testing strategy. Nothing here duplicates an item above or in the 2026-08-03
additions; where they touch, the note says so.

### P0

### P1

### P2

### P3

