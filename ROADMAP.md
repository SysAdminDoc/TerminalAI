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

- [ ] P2 — Let a launch bound its own fan-out
  Why: admission governs sessions, spend and memory, but a single session can now spawn a team and up to 20 concurrent subagents — the one resource multiplier the launcher cannot express. Depends on the job-memory item above, whose process count is the only way to see whether a cap took effect.
  Evidence: `CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS` (default 20, Claude Code 2.1.216), `CLAUDE_CODE_MAX_SUBAGENTS_PER_SESSION` (default 200, 2.1.212), `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` (https://code.claude.com/docs/en/agent-teams). None appears in `safe_environment_keys()` (`crates/terminalai-core/src/environment.rs:325`), a 17-entry Windows allowlist — correct by default, but it means the caps are reachable only through the generic passthrough and nothing in the fleet's own governance knows about them.
  Touches: `crates/terminalai-core/src/launch.rs`, `crates/terminalai-core/src/environment.rs`, `web/src/main.js`
  Acceptance: a launch can set the concurrent-subagent cap and enable or refuse agent teams explicitly; the value reaches the previewed argv or environment; Codex, which has no equivalent, is refused rather than silently launched as if it had one. The row's process count from the job-memory item is what proves the cap took effect.
  Complexity: M

- [ ] P2 — Split the daemon's control plane the way the registry was split
  Why: `lib.rs` is the tree's largest Rust file and fuses the wire protocol, the dispatcher, the server, the client and the console handler in one scope — worth splitting before it reaches the size that forced the registry split, not after.
  Evidence: `crates/terminalai-daemon/src/lib.rs` is 3,254 lines, of which roughly a third is its test module. It is not yet the 6,106 lines `CHANGELOG.md:524` records for `registry.rs` when that was decomposed, which is the point; the seams are already visible as `Request`/`Response`, `dispatch_with_endpoint`, `DaemonServer`, `DaemonClient`. The precedent, including the moved-code-unchanged discipline and the cross-language string-reading tests, is `crates/terminalai-core/src/registry/` and `web/tests/registrySource.mjs`.
  Touches: `crates/terminalai-daemon/src/lib.rs` split into `protocol.rs` / `dispatch.rs` / `client.rs`, `web/tests/registrySource.mjs` if any test reads the file by name
  Acceptance: `lib.rs` is under ~1,200 lines with the moved code unchanged, both suites pass without assertion edits, and the private items the dispatcher relies on stay private rather than being widened to make the split possible.
  Complexity: L

- [ ] P2 — Translate the launcher's advanced disclosure
  Why: the i18n gate enforces that every catalogued message is referenced and every `data-i18n` attribute is handled, but it cannot see literal text with no attribute at all — so 86 user-facing strings sit outside the catalog while coverage reports clean.
  Evidence: `web/index.html:245-282` — the `Agent`, `Permission mode`, `Sandbox`, `Resume`, `Native session id`, `Enable web search`, `Extra writable folders` and `Use only those MCP servers` labels, 13 `<option>` values, 23 `<em>` hints, 29 label spans and 21 `placeholder=` attributes in that block carry no `data-i18n` — 86 strings in total. `web/tests/i18n.test.mjs` asserts the reverse direction only. Three strings in `web/src/main.js` (lines 1821, 2890, 2909) also bypass `t()`.
  Touches: `web/index.html`, `web/src/i18n/terminalai.ftl`, `web/src/main.js`, `web/tests/i18n.test.mjs`
  Acceptance: every user-facing string in the launcher goes through the catalog, and the i18n test fails when a text-bearing element inside a dialog has no `data-i18n` attribute and no runtime writer — the check that would have caught this.
  Complexity: M

### P3

- [ ] P3 — Close the gap between what the probe dispatches and what it documents
  Why: three subcommands are reachable but missing from the binary's own help, and one of them is undocumented everywhere — which is how a harness accumulates commands nobody knows are there.
  Evidence: `crates/terminalai-probe/src/main.rs:89-118` dispatches 29 subcommands; the `USAGE` constant at `:30-84` lists 26. `auth`, `exec` and `limits` are absent from it; `exec` and `limits` at least appear in `README.md` (`:465`, `:185`), so only `auth` is undocumented anywhere. Separately, eleven subcommands `USAGE` does list (`broadcast`, `queue`, `land`, `pin`, `grid`, `history`, `search`, `archives`, `archive`, `worktrees`, `verify-goldens`) appear nowhere in `README.md`.
  Touches: `crates/terminalai-probe/src/main.rs`, `README.md`
  Acceptance: `USAGE` is derived from the dispatch table rather than hand-maintained beside it, so a new subcommand cannot be added without appearing in help; the README's probe section names every command or says explicitly which are internal.
  Complexity: S

- [ ] P3 — Delete the code with no callers, or wire it up
  Why: eight public functions have no references anywhere, tests included. None can produce a wrong answer — nothing calls them — which is the argument for deleting rather than maintaining them.
  Evidence: `SessionStatus::colour()` (`crates/terminalai-core/src/session.rs:112`) is a second status-to-colour table that no caller consults while the frontend uses its own `STATUS_META`; `in_state_for`/`in_status_for` (`session.rs:890`, `:898`); `PriceTable::with_model()` (`transcript.rs:107`); `agent_auth`/`auth_holds` (`registry/mod.rs:522`, `:530`); `from_store_with_domain` (`:401`); `forget_transcript` (`registry/sampling.rs:220`). Separately, `resolve_agent` is the only one of 69 Tauri commands never invoked from `web/` — the frontend gets the same answer from `agent_capabilities` and `preview_launch`.
  Touches: `crates/terminalai-core/src/session.rs`, `transcript.rs`, `registry/mod.rs`, `registry/sampling.rs`, `crates/terminalai-app/src/main.rs`, `crates/terminalai-app/build.rs`, the three capability files
  Acceptance: each is removed. Keeping any of them requires a caller in the same change, and wiring `colour()` through to the frontend is a separate item with its own estimate, not a way to close this one. Removing `resolve_agent` also removes its manifest entry and all three ACL grants, which the build-time gate already enforces.
  Complexity: S
