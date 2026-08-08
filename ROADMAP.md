# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; completed items are removed.

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

- [ ] P3 — Scheduled work runs
  Why: the work queue already runs one stored prompt across many projects on demand; running it on a schedule is the natural extension, and it is the one automation feature the larger field ships that fits this tool's shape.
  Evidence: `crates/terminalai-core/src/work_queue.rs` and `crates/terminalai-app/src/work.rs` implement the on-demand run including dirty-tree refusal and restart survival. superset ships Automations for exactly this — https://docs.superset.sh/automations
  Touches: `crates/terminalai-core/src/work_queue.rs`, `crates/terminalai-daemon/src/lib.rs`, `crates/terminalai-app/src/work.rs`, `web/src/main.js`
  Acceptance: a work run can be scheduled and survives a daemon restart; a scheduled run inherits every existing refusal (dirty tree, admission, spend ceiling) rather than bypassing them, and reports what it did while the operator was away.
  Complexity: M

## Research-Driven Additions — 2026-08-04

Second external research pass (see `RESEARCH.md`), covering ground the first did not: process-
supervision and admission-control theory, Windows distribution and supply chain, VT/terminal
correctness, and testing strategy. Nothing here duplicates an item above or in the 2026-08-03
additions; where they touch, the note says so.

### P0

### P1

### P2

### P3

