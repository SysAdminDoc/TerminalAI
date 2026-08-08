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
  Where: `crates/terminalai-app/src/main.rs`, `crates/terminalai-app/src/preflight.rs`, `web/src/main.js`, `web/src/sessionStatus.js`, `web/src/reviewPage.js`, `web/src/styles.css`, `web/tests/`.
  Problem: the frontend shell is below its decomposition target, but source-slicing tests and mixed lifecycle/event ownership still make safe refactors harder.
  Evidence: work-run/schedule orchestration now lives in `workflows.rs`; preflight, URL safety and shortcut repair now live in `preflight.rs`; daemon event fan-out, taskbar/toast state and output batching now live in `events.rs`; browser event wiring now lives in `eventBindings.js`; workspace-page handlers are returned and covered at their module boundary; fleet status semantics now live in `sessionStatus.js`; review rendering and landing actions now live in `reviewPage.js`; review/error/landing/cost source assertions now read the owning module; the stylesheet is split into `tokens.css`, `foundation.css`, `pages.css`, and `shell.css`; `appRustSource.mjs` and `cssSource.mjs` cover complete assembled modules. Remaining source-slice assertions still need migration.
  Fix: extract remaining app command/lifecycle groups and migrate source-grep assertions toward exported pure functions, DOM fixtures and protocol tests.
  Touches: `crates/terminalai-app/src/main.rs`, `crates/terminalai-app/src/`, `web/src/main.js`, `web/src/styles.css`, `web/tests/`.
  Acceptance: no entry module owns unrelated feature families, tests assert behavior at module boundaries rather than string position, and the existing Rust/frontend/chrome gates remain unchanged and green.
  Complexity: L

- [ ] P2 — Make distribution and cross-target verification repeatable
  Category: release engineering
  Where: installer scripts, GitHub release workflow, winget metadata and cross-target checks.
  Problem: a successful local build is not yet a frictionless release path for users, and Windows-only verification leaves Unix cfg branches and published installer metadata outside the regular gate.
  Evidence: `Roadmap_Blocked.md` tracks the outward-facing winget/checksum steps and the need for cross-host closure of non-Windows branches.
  Fix: automate release artifact manifests and SHA-256 checks, publish a ready-to-submit winget manifest, run Linux/macOS compile/test jobs for cfg branches, and keep the unsigned installer policy explicit.
  Touches: `.github/workflows/`, `scripts/verify-installer.ps1`, `scripts/verify-reproducible.ps1`, `scripts/check-cross-targets.ps1`, release metadata.
  Acceptance: one tagged release produces reproducible NSIS/MSI artifacts, hashes, upgrade evidence and submission-ready metadata; cross-target CI compiles/tests the non-Windows branches and reports unsupported runtime checks honestly.
  Complexity: M

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
