# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; completed items are removed.

## Improvement Program — 2026-08-08

This is the next product and engineering pass after v0.23.0. The visual redesign and frontend
decomposition are complete; the remaining work closes the gap between strong internal contracts,
the packaged Windows application, real agent versions, and an unfamiliar operator's first run.
Items stay here while their implementation is actionable. Operator-owned or vendor-owned evidence
remains cross-referenced in `Roadmap_Blocked.md` and is not claimed by a local test.

- [ ] P2 — Finish shell maintainability and replace brittle source contracts
  Category: maintainability
  Where: `crates/terminalai-app/src/main.rs`, `crates/terminalai-app/src/state.rs`, `crates/terminalai-app/src/preflight.rs`, `web/src/main.js`, `web/src/sessionStatus.js`, `web/src/reviewPage.js`, `web/src/externalSessions.js`, `web/src/broadcastPanel.js`, `web/src/rollupPage.js`, `web/src/explainerPage.js`, `web/src/sessionFocus.js`, `web/src/snapshotCoordinator.js`, `web/src/settingsPage.js`, `web/src/fleetSummary.js`, `web/src/pinnedPanes.js`, `web/src/sessionDemo.js`, `web/src/terminalHeader.js`, `web/src/sessionState.js`, `web/src/terminalLayout.js`, `web/src/fleetRowState.js`, `web/src/fleetGrouping.js`, `web/src/fleetList.js`, `web/src/rendererUtils.js`, `web/src/sessionPresentation.js`, `web/src/shellNavigation.js`, `web/src/fleetNotices.js`, `web/src/firstRunGuide.js`, `web/src/reviewVisibility.js`, `web/src/updatePanel.js`, `web/src/shellModes.js`, `web/src/startup.js`, `web/src/styles.css`, `web/tests/`.
  Problem: the frontend shell is below its decomposition target, but source-slicing tests and mixed lifecycle/event ownership still make safe refactors harder.
  Evidence: shared Tauri state and serialized command payloads now live in `state.rs`, separating storage ownership and response schemas from the entry module; work-run/schedule orchestration now lives in `workflows.rs`; preflight, URL safety and shortcut repair now live in `preflight.rs`; daemon event fan-out, taskbar/toast state and output batching now live in `events.rs`; browser event wiring now lives in `eventBindings.js`; workspace-page handlers are returned and covered at their module boundary; fleet status semantics now live in `sessionStatus.js`; review rendering and landing actions now live in `reviewPage.js`; external-session rendering and lookup now live in `externalSessions.js`; broadcast rendering and send/refusal handling now live in `broadcastPanel.js`; spend rollup rendering and dialog entry now live in `rollupPage.js`; the row-model explainer now lives in `explainerPage.js`; serialized focus switching and stale-route restoration now live in `sessionFocus.js`; snapshot refresh, event replay, preflight outage routing and terminal reattachment now live in `snapshotCoordinator.js`; admission settings loading, validation, usage cards and save/refresh behavior now live in `settingsPage.js`; fleet summary accounting and pinned grid-preview reconciliation now live in `fleetSummary.js` and `pinnedPanes.js`; offline demo state swapping and terminal identity/placeholder updates now live in `sessionDemo.js` and `terminalHeader.js`; daemon session reconciliation, attention toasts and coalesced status announcements now live in `sessionState.js`; focused-terminal measurement, debounced resizing and per-session geometry delivery now live in `terminalLayout.js`; fleet row creation, keyboard movement, action dispatch and incremental DOM updates now live in `fleetRowState.js`; urgency-aware grouping, structured filters and group-chip rendering now live in `fleetGrouping.js`; fleet list reconciliation, empty/loading states and interaction-safe priority ordering now live in `fleetList.js`; renderer escaping, terminal byte routing, stale-output guards and error surfaces now live in `rendererUtils.js`; session cost, memory, dwell and answer-deadline presentation now live in `sessionPresentation.js`; rail/dialog navigation and first-run, review, update, mode and startup coordination now live in `shellNavigation.js`, `firstRunGuide.js`, `reviewVisibility.js`, `updatePanel.js`, `shellModes.js` and `startup.js`; store/auth notices live in `fleetNotices.js`; returned row actions, grouping/filter contracts, list affordances and visual states are covered at their owning boundaries; feature source assertions now use `moduleSource()` and `appSource()` remains only for complete-renderer invariants such as catalog/attribute safety; the stylesheet is split into `tokens.css`, `foundation.css`, `pages.css`, and `shell.css`; `appRustSource.mjs` and `cssSource.mjs` cover complete assembled modules. The remaining maintainability work is the app command/lifecycle seam and its boundary tests.
  Fix: extract the remaining app command/lifecycle groups and replace their broad source contracts with command/protocol tests while preserving the green Rust/frontend/chrome gates.
  Touches: `crates/terminalai-app/src/main.rs`, `crates/terminalai-app/src/`, `web/src/main.js`, `web/src/styles.css`, `web/tests/`.
  Acceptance: no entry module owns unrelated feature families, tests assert behavior at module boundaries rather than string position, and the existing Rust/frontend/chrome gates remain unchanged and green.
  Complexity: L

## Audit Findings — 2026-08-07

Fourth audit pass, against `cc53821` / v0.15.0 with a green baseline of 585 Rust (0 failed), 305
frontend (0 failed), clippy clean, and the 13-surface chrome gate clean in both themes at both
widths. No pre-existing test, lint or build failure exists. Verification: every code-path finding
below was traced to a real caller; the theming finding was observed live (Vite + headless Chromium,
both `prefers-color-scheme` values) rather than inferred from source.

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

## Research-Driven Additions — 2026-08-08

Filed from the research pass in `RESEARCH.md` (2026-08-08), against v0.21.0 with a green baseline.
Nothing here re-files an item that already exists above or in `Roadmap_Blocked.md`. One item is
deliberately about *unblocking* two entries in that file and names them rather than copying them;
where an item depends on another in this section, its Why says which.

### P0

### P1

### P2

### P3
