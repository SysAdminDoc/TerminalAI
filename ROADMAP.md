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
  Where: `web/src/main.js` (~3,690 lines, 151.5 KB)
  Problem: main.js is now the tree's god file — the position `registry.rs` held before the 2026-08-07 decomposition, and the highest-churn JS file. Eleven modules have already been split out (rowMarkup, rateLimit, rollup, menus, fleetRows, …), which proves the seams work; what remains fuses dialog wiring, terminal setup, update checking, preflight, diagnostics, i18n bootstrapping and the event loop in one scope where every edit risks every feature.
  Evidence: `wc -l` = 3,686; the extraction pattern and its per-module tests already exist and are green.
  Fix: continue the established pattern, one seam at a time, each with the moved-code-unchanged discipline used for `rowMarkup.js`: (1) ~~`updateCheck.js`~~ — landed 2026-08-07 with eight tests over the version comparison and every outcome branch; (2) `terminalPane.js` (setupTerminal/useWebglRenderer/observeTerminalSize/openSessionLink), (3) `dialogs.js` (open/close/focus-restore wiring shared by the nine dialogs). NOT a big-bang rewrite; run BOTH suites after every move — several frontend tests read `main.js` itself as a string and break silently on a move (see CLAUDE.md 2026-08-07).
  Acceptance: main.js under ~2,500 lines with the remaining two modules extracted (it is ~3,710 after the first), every frontend test passing without assertion edits (string-reading tests may need their read target updated, mirroring `registrySource.mjs`), and the chrome gate clean.
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

