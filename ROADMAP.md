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

## Research-Driven Additions — 2026-08-08

Filed from the research pass in `RESEARCH.md` (2026-08-08), against v0.21.0 with a green baseline.
Nothing here re-files an item that already exists above or in `Roadmap_Blocked.md`. One item is
deliberately about *unblocking* two entries in that file and names them rather than copying them;
where an item depends on another in this section, its Why says which.

### P0

### P1

### P2

- [ ] P2 — Split the daemon's control plane the way the registry was split
  Why: `lib.rs` is the tree's largest Rust file and fuses the wire protocol, the dispatcher, the server, the client and the console handler in one scope — worth splitting before it reaches the size that forced the registry split, not after.
  Evidence: `crates/terminalai-daemon/src/lib.rs` is 3,254 lines, of which roughly a third is its test module. It is not yet the 6,106 lines `CHANGELOG.md:524` records for `registry.rs` when that was decomposed, which is the point; the seams are already visible as `Request`/`Response`, `dispatch_with_endpoint`, `DaemonServer`, `DaemonClient`. The precedent, including the moved-code-unchanged discipline and the cross-language string-reading tests, is `crates/terminalai-core/src/registry/` and `web/tests/registrySource.mjs`.
  Touches: `crates/terminalai-daemon/src/lib.rs` split into `protocol.rs` / `dispatch.rs` / `client.rs`, `web/tests/registrySource.mjs` if any test reads the file by name
  Acceptance: `lib.rs` is under ~1,200 lines with the moved code unchanged, both suites pass without assertion edits, and the private items the dispatcher relies on stay private rather than being widened to make the split possible.
  Complexity: L

### P3
