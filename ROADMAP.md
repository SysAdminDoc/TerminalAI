# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; completed items are removed.

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

- [ ] P2 — Extract the row renderers out of `main.js`
  Why: `styles.css` was un-minified on 2026-08-04 and is now one declaration per line, but `main.js` still carries 62 lines over 120 columns — 3,506 lines with a longest line of 1,348 — and those are the renderers, where a typo is least visible and most load-bearing.
  Evidence: `web/tests/lineLength.test.mjs` holds `MAIN_JS_LONG_LINE_BUDGET = 62` as a ratchet; every other file under `web/src/` is already inside the limit. The long lines are single-expression HTML templates (`renderRows`, the fleet row `<article>` at `main.js:1447-1453`, the wide-meta and reply blocks), so mechanical rewrapping changes the string a template produces rather than only its layout. Five extracted modules (`broadcast.js`, `fleetRows.js`, `projects.js`, `rateLimit.js`, `rollup.js`) establish the pattern.
  Touches: `web/src/main.js`, new modules under `web/src/`, `web/tests/lineLength.test.mjs`, the tests that slice `main.js` by function name
  Acceptance: the row and wide-meta renderers live in their own modules within the 120-column limit, the ratchet in `lineLength.test.mjs` drops to what remains, and the rendered markup is unchanged — verified by driving the UI, not by reading the diff.
  Complexity: M

- [ ] P2 — An approvals inbox across the fleet
  Why: permission decisions are the fleet's blocking work and are currently answered one focused session at a time; the field has converged on a single queue, and Claude Code exposes the hook needed to answer programmatically.
  Evidence: `SessionStatus::NeedsApproval` exists (`crates/terminalai-core/src/session.rs:73`) but no aggregate surface does. `PermissionRequest`/`PermissionDenied` are already managed in `hook_config.rs` and support `updatedInput` and `retry` (code.claude.com/docs/en/hooks). Requested at anthropics/claude-code#58247; shipped by wmux and octomux (https://github.com/ShreyPaharia/octomux).
  Touches: `crates/terminalai-core/src/hooks.rs`, `crates/terminalai-core/src/registry.rs`, `crates/terminalai-daemon/src/lib.rs`, `web/index.html`, `web/src/main.js`
  Acceptance: one dialog lists every pending permission request across sessions with its tool and arguments, and answering routes to the right session. It never auto-approves and never enables bypass mode on the operator's behalf — cmux was criticised for exactly that (https://github.com/manaflow-ai/cmux/issues/3547).
  Complexity: L
  Note (2026-08-06 research): hooks may not be the right mechanism. Claude Code documents `--permission-prompt-tool`, which designates an MCP tool to handle permission prompts, and this project already ships an MCP server (`crates/terminalai-core/src/mcp.rs`) — so the sanctioned route is to point the launched session at our own server rather than to intercept `PermissionRequest` in `hooks.rs`. Evaluate that first; it is fewer moving parts and it survives `disableAllHooks`. See https://code.claude.com/docs/en/cli-reference.

- [ ] P2 — Show context pressure, and distinguish context exhaustion from refusal
  Why: compaction and context loss are a top-tier community complaint, and a session approaching its window is about to lose quality in a way no current row state predicts.
  Evidence: Claude Code's statusline payload carries `context_window.used_percentage`, `remaining_percentage` and `exceeds_200k_tokens` (code.claude.com/docs/en/statusline); Codex config exposes `model_context_window` and `model_auto_compact_token_limit`. Nothing in `crates/terminalai-core/src/session.rs` models context. Asked directly at https://github.com/Untrivial-ai/agent-orchestrator/issues/3322; community evidence at openai/codex#4106, #11325.
  Touches: `crates/terminalai-core/src/transcript.rs`, `session.rs`, `web/src/main.js`, `web/src/i18n/terminalai.ftl`
  Acceptance: the wide row shows context used against the model's window when the agent reports it, an em dash when it does not, and a compaction event is visible in the status history rather than appearing as an unexplained pause.
  Complexity: M
  Note (2026-08-06 research): compaction is now operator-controllable, so the row can show the threshold and not just the usage. Claude Code added `--autocompact <auto|tokens>` in v2.1.221 and the `CLAUDE_CODE_AUTO_COMPACT_WINDOW` variable (100000–1000000 tokens); Codex exposes `model_auto_compact_token_limit` alongside `model_context_window`. Both are launch-time inputs this launcher does not map, so the same work can set the threshold it displays.

- [ ] P2 — Search the focused pane and the on-disk scrollback
  Why: the project keeps a 512 KB ring over an 8 MB rotating spool per session and offers no way to query either; "where did that error print" across twenty sessions is currently a manual scroll.
  Evidence: `web/package.json` imports `@xterm/addon-fit`, `-unicode11` and `-webgl` only — no `addon-search`. `crates/terminalai-core/src/scrollback.rs` writes segments no reader queries. `@xterm/addon-search` 0.16.0 ships a `SearchLineCache` making 6.0 materially faster (https://github.com/xtermjs/xterm.js/releases).
  Touches: `web/package.json`, `web/src/main.js`, `crates/terminalai-core/src/scrollback.rs`, `crates/terminalai-daemon/src/lib.rs`, `crates/terminalai-app/src/main.rs`
  Acceptance: find-in-pane with match count in the focused terminal, plus a fleet search that reports which sessions match a string in their retained scrollback and how many times. `rendererCapabilities.test.mjs` gains the addon to its explicit allowlist.
  Complexity: M

- [ ] P2 — Move the MCP server to the current protocol revision
  Why: the server declares a revision two behind and implements a handshake the current spec removed, so a conforming 2026 client cannot negotiate with it.
  Evidence: `crates/terminalai-core/src/mcp.rs:35` declares `PROTOCOL_VERSION = "2025-06-18"` and `:308` returns it from `initialize`. The current revision is 2026-07-28, which removes `initialize`/`notifications/initialized`, moves version and capabilities into `_meta`, adds a mandatory `server/discover`, and replaces server-initiated elicitation with Multi Round-Trip Requests — https://modelcontextprotocol.io/specification/2026-07-28/changelog
  Touches: `crates/terminalai-core/src/mcp.rs`, `crates/terminalai-core/tests/mcp_server.rs`
  Acceptance: the server answers `server/discover`, negotiates via `_meta`, and returns `resultType` on every result; a test pins the declared revision against the constant so the two cannot drift. Decide explicitly whether to keep the old handshake for one release.
  Complexity: M

- [ ] P2 — Pass through the launcher flags that control tools, MCP and plugins
  Why: permission-prompt fatigue is a top community complaint and `--allowed-tools`/`--disallowed-tools` is the precise lever for it; passing plugin and MCP flags was already identified as the right extension point and never shipped.
  Evidence: `crates/terminalai-core/src/launch.rs` maps 18 flags; absent are `--allowed-tools`, `--disallowed-tools`, `--settings`, `--setting-sources`, `--mcp-config`, `--strict-mcp-config`, `--plugin-dir`, `--plugin-url`, `--fallback-model`, `--max-turns` (code.claude.com/docs/en/cli-reference). The prior research pass rejected a plugin architecture but explicitly endorsed flag passthrough as belonging "in the existing flag-mapping table". Permission fatigue: https://news.ycombinator.com/item?id=48308376 (386 points).
  Touches: `crates/terminalai-core/src/launch.rs`, `crates/terminalai-core/src/capabilities.rs`, `crates/terminalai-app/src/preset.rs`, `web/index.html`, `web/src/main.js`, `crates/terminalai-core/tests/launch_golden.rs`
  Acceptance: each flag is mapped per agent through the existing table and refuses with `LaunchError::Unsupported` where the chosen agent has no equivalent; golden fixtures cover the new argv. Also decide deliberately what to do about Claude Code's own `--worktree`, which now overlaps this project's worktree feature.
  Complexity: M

### P3

- [ ] P3 — Say when the vendored price table is stale
  Why: prices are embedded from a pinned upstream commit and nothing ages them, so a table months out of date reports spend with the same confidence as a current one.
  Evidence: `crates/terminalai-core/pricing/model-prices.json` carries `source_commit`, `source_committed: "2026-07-31"` and `retrieved: "2026-08-03"`; `transcript.rs:168` embeds it with `include_str!`. Nothing compares those dates to now. The offline-by-design decision in `README.md` is correct and should be preserved.
  Touches: `crates/terminalai-core/src/transcript.rs`, `web/src/main.js`, `web/src/rollup.js`
  Acceptance: the existing price-table tooltip states the table's age, and past a stated threshold the rollup marks figures as computed against a stale table. Nothing is fetched at runtime.
  Complexity: S

- [ ] P3 — Ingest the remaining hook events that change what a row means
  Why: sixteen events are managed and the unhandled ones include the two that silently invalidate a row's contents and the one that would let this tool own agent-created worktrees.
  Evidence: `crates/terminalai-core/src/hook_config.rs` manages SessionStart/End, Pre/PostToolUse, PostToolUseFailure, PostToolBatch, Pre/PostCompact, Permission{Request,Denied}, Stop, StopFailure, Subagent{Start,Stop}, UserPromptSubmit, Notification. Not handled: `CwdChanged` and `DirectoryAdded` (the row's folder and branch go stale), `WorktreeCreate` (can return a `worktreePath`, letting the supervisor place it), `TaskCreated`/`TaskCompleted` (tool progress beyond `TodoWrite`), `Elicitation`/`ElicitationResult`, `MessageDisplay`, `UserPromptExpansion`, `InstructionsLoaded`, `ConfigChange` — code.claude.com/docs/en/hooks
  Touches: `crates/terminalai-core/src/hook_config.rs`, `hooks.rs`, `registry.rs`, `session.rs`
  Acceptance: `CwdChanged` updates the row's folder and branch; `WorktreeCreate` returns a path under this tool's worktree root; each newly ingested event appears in the status history with its source. Events deliberately not ingested are listed with a reason.
  Complexity: M

- [ ] P3 — Corroborate cost from OpenTelemetry rather than only from an unsupported transcript format
  Why: Anthropic documents the transcript entry format as internal and version-changing, so the entire cost path rests on a contract that is explicitly not promised, while a sanctioned metrics surface exists.
  Evidence: code.claude.com/docs/en/sessions states the entry format "is internal to Claude Code and changes between versions". code.claude.com/docs/en/monitoring-usage defines `claude_code.cost.usage` and `claude_code.token.usage` with `query_source` (`main|subagent|auxiliary`) attribution; Codex exposes `[otel]` in config. Note both are client-side estimates, not bills.
  Touches: `crates/terminalai-core/src/transcript.rs`, `crates/terminalai-daemon/src/lib.rs`, `crates/terminalai-core/src/hook_config.rs`
  Acceptance: when an OTel endpoint is configured the fleet prefers it and says so in the price-table tooltip, falling back to transcript arithmetic otherwise; a disagreement between the two beyond a threshold is logged rather than silently resolved. Gated on the open question in `RESEARCH.md` about enabling OpenTelemetry export on a subscription plan.
  Complexity: L

- [ ] P3 — Declare per-monitor-v2 DPI awareness for the Rust side
  Why: this machine runs 125% scaling, and any Rust-side code that enumerates monitors or captures pixels gets virtualized numbers unless awareness is declared before the first such call.
  Evidence: no manifest and no `SetProcessDpiAwarenessContext` call exists in `crates/terminalai-app/`; the process inherits whatever Tauri/wry sets. The screenshot and visual-isolation tooling this repo's release gate depends on already has to declare it independently — learn.microsoft.com/en-us/windows/win32/hidpi/setting-the-default-dpi-awareness-for-a-process
  Touches: `crates/terminalai-app/src/main.rs`, `crates/terminalai-app/build.rs` (manifest), `scripts/verify-installer.ps1`
  Acceptance: awareness is declared explicitly rather than inherited, and a diagnostic reports the process's DPI context so a wrong value is visible instead of silently wrong.
  Complexity: S

- [ ] P3 — Add the repository's own `.github/` surface
  Why: the repo is public with no issue templates and no in-repo security contact, so the first external bug report and the first vulnerability report both arrive unstructured.
  Evidence: `ls -a .github` returns nothing; there is no `SECURITY.md`, `CONTRIBUTING.md` or `docs/` directory. Community health files are served org-wide from `SysAdminDoc/.github`, which covers conduct and contributing but not repository-specific triage or a security contact.
  Touches: `.github/ISSUE_TEMPLATE/`, `SECURITY.md`
  Acceptance: a bug template that asks for the daemon log path, agent versions and Windows build — the three facts every finding in this roadmap needed. If lint/test CI is wanted, the repository owner's standing rule permits validation workflows but forbids building or releasing binaries there; the release gate stays local either way.
  Complexity: S

- [ ] P3 — Make the localization scaffolding do something
  Why: the Fluent machinery, a shared catalog and a Rust-side duplicate-key check are all built for exactly one locale that is also the fallback, so the abstraction currently costs maintenance and returns nothing.
  Evidence: `crates/terminalai-core/src/i18n.rs:11` hardcodes `DEFAULT_LOCALE = "en-US"`; `web/src/i18n/` contains a single `terminalai.ftl`. The existing P3 i18n item above covers catalog hygiene (orphaned keys, hardcoded strings) but not locale coverage or negotiation.
  Touches: `crates/terminalai-core/src/i18n.rs`, `web/src/i18n.js`, `web/src/i18n/`
  Acceptance: either a second locale plus OS-preference negotiation with a documented fallback chain, or a recorded decision that the project ships English only — in which case say so in `README.md` and stop paying for the abstraction.
  Complexity: M

- [ ] P3 — Restart cleanly after a crash or a Windows update
  Why: the daemon is designed to outlive its window, but nothing tells Windows how to bring the app back, so a forced restart drops the operator into an empty desktop with live sessions still running.
  Evidence: no `RegisterApplicationRestart` call exists in `crates/`. The daemon already reattaches live rows and replays bounded scrollback on reconnect (`README.md`), so the reattach path this would trigger is built and tested — learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-registerapplicationrestart
  Touches: `crates/terminalai-app/src/main.rs`
  Acceptance: after a forced restart the app relaunches and reattaches to the running daemon's sessions, and the restart command line is covered by the installer gate.
  Complexity: S

- [ ] P3 — Show agent-reported progress on the taskbar
  Why: the overlay-icon half of the taskbar integration already ships; the progress half is missing its input, and an addon that decodes it exists.
  Evidence: `update_taskbar_waiting_count` (`crates/terminalai-app/src/main.rs:1682`) sets an overlay icon only. `@xterm/addon-progress` 0.2.0 parses ConEmu's `OSC 9;4` progress sequence, new in the xterm 6.0 cycle (https://github.com/xtermjs/xterm.js/releases); `ITaskbarList3::SetProgressValue` is the sink.
  Touches: `web/package.json`, `web/src/main.js`, `crates/terminalai-app/src/main.rs`, `web/tests/rendererCapabilities.test.mjs`
  Acceptance: an agent emitting `OSC 9;4` drives a taskbar progress bar; agents that emit nothing leave it absent rather than showing a fabricated value.
  Complexity: M

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

- [ ] P2 — A VT conformance corpus for the grid
  Why: `grid.rs` has property tests but no conformance cases, and it is the renderer for the pinned split view — so every divergence from a real terminal is currently invisible until someone looks at a pane.
  Evidence: alacritty's ref-test harness is raw recording bytes plus `size.json` and a serialized `grid.json`, replayed in-process with `parser.advance()` — no GUI, no PTY: https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/tests/ref.rs . Its case list covers exactly this grid's untested areas (`vttest_origin_mode_1/2`, `vttest_insert`, `vttest_scroll`, `vttest_tab_clear_set`, `saved_cursor_alt`, `wrapline_alt_toggle`, `zerowidth`, `csi_rep`, `decaln_reset`, plus real captures `tmux_htop`, `vim_*`). Apache-2.0, so the corpus is reusable with attribution.
  Touches: `crates/terminalai-core/tests/grid_ref.rs`, new fixture directory under `crates/terminalai-core/tests/fixtures/`
  Acceptance: recorded byte streams replay into the grid and compare against a serialized expected grid; the zero-width and resize fixes above are covered by cases rather than by hand-written assertions. `esctest2` is explicitly not adopted — it reads cells back via DECRQCRA, which `vte` 0.15 does not dispatch.
  Complexity: M

- [ ] P2 — Model-based tests over the registry and session store
  Why: 411 example-based tests are structurally blind to the failure this program is most exposed to — an *ordering* of individually legal operations whose end state diverges from what the model says it should be.
  Evidence: `proptest` is already a workspace dependency and used in exactly one module (`crates/terminalai-core/src/grid.rs`). `proptest-state-machine` provides `ReferenceStateMachine` + `StateMachineTest` + `prop_state_machine!` and lives in the proptest repo — https://docs.rs/proptest-state-machine/ . The surface under test — create, kill, restart, archive, lease, release, persist, reload, quarantine — is `registry.rs` (4,545 lines) plus `store.rs`.
  Touches: `crates/terminalai-core/tests/`, `crates/terminalai-core/Cargo.toml`
  Acceptance: a reference model with no threads, PTY or disk runs alongside the real registry and store over generated operation sequences; a divergence shrinks to a minimal reproducing sequence. Start sequential-mode only.
  Complexity: L

- [ ] P2 — A mock clock for the time-dependent state machines
  Why: backoff, lease expiry, rate-limit windows, dwell and notification grace are all time-driven, and `registry.rs` alone makes 52 time calls — so those behaviours are currently either sleep-based tests or untested.
  Evidence: `mock_instant` provides drop-in `Instant`/`SystemTime` with `MockClock::advance()` — https://crates.io/crates/mock_instant . The repo has already been bitten by clock-granularity assumptions: the 2026-08-03 `CLAUDE.md` entry on two files sharing a 15.6 ms tick, and the `BIRTH_GRACE` constant that exists to paper over it.
  Touches: `crates/terminalai-core/src/registry.rs`, `session.rs`, `notification.rs`, `lease.rs`, `Cargo.toml`
  Acceptance: restart backoff, the restart window, lease expiry and notification grace are asserted by advancing a mock clock rather than sleeping; no test in those areas calls `thread::sleep`. Prerequisite for the windowed restart budget being testable at all.
  Complexity: M

- [ ] P2 — Split the supervisor's health verdict from "is it running"
  Why: a session that is busy thinking and one that has wedged are the same thing to the supervisor, which is the documented cause of restart storms in every system that conflates them.
  Evidence: `SessionHealth` (`crates/terminalai-core/src/session.rs:135-141`) is recomputed from `status` and `pid` on every transition (`:386-392`), so it carries no independent signal. Nothing detects a wedged pty reader, though `pty.rs:110` and `registry.rs:2728` both document that blocking there stalls the fleet. Kubernetes separates liveness, readiness and startup probes and requires `failureThreshold` consecutive failures (default 3) before acting — https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/ ; systemd's `EXTEND_TIMEOUT_USEC` lets a slow-but-alive service push its own deadline out — https://man7.org/linux/man-pages/man3/sd_notify.3.html
  Touches: `crates/terminalai-core/src/session.rs`, `crates/terminalai-core/src/registry.rs`
  Acceptance: progress signals (pty output, transcript growth, hook events) extend a per-session deadline; missing it repeatedly marks the session unhealthy without restarting it, and only a proven-dead process restarts. Pairs with the "Detect a stalled session" item from 2026-08-03, which surfaces this to the operator — this item is the supervisor-side verdict that item displays.
  Complexity: M

### P3

- [ ] P3 — Close the coverage gaps the first `llvm-cov` run named
  Why: coverage was run once on 2026-08-04 (72.15% regions, 71.28% lines workspace-wide) to find untested arms rather than to chase a number, and it named two modules whose low figures are not explained by being entry points.
  Evidence: `crates/terminalai-daemon/src/lib.rs` is at **58.07%** line coverage — the control plane, where every request arm and every framing error lives — and `crates/terminalai-daemon/src/logging.rs` at **31.29%**. `terminalai-daemon/src/main.rs` (0%) and `terminalai-probe/src/main.rs` (12%) are process entry points and a CLI harness, and are not the target. Re-run with `pwsh`-free `RUSTC=<managed>/bin/rustc.exe cargo-llvm-cov llvm-cov --workspace --summary-only`; note the suite only runs under coverage because `lease_command_uses_the_allowlist_without_putting_connection_in_argv` now ignores the profiling runtime's own `__LLVM_PROFILE*` variables.
  Touches: `crates/terminalai-daemon/src/lib.rs`, `crates/terminalai-daemon/src/logging.rs`, `crates/terminalai-daemon/tests/`
  Acceptance: the uncovered request arms and error branches in the daemon's control plane are either covered or individually recorded as unreachable with a reason. The number itself is not the goal and must not become one.
  Complexity: M

- [ ] P3 — Snapshot-test the golden argv and grid renders
  Why: the launch golden fixtures and grid reference output are hand-maintained string literals, so a deliberate change to either means editing expectations by hand and a drift means editing them wrongly.
  Evidence: `crates/terminalai-core/tests/launch_golden.rs` and `grid_ref.rs` exist as hand-written comparisons. `insta` provides reviewable file snapshots with `cargo insta test --review` — https://insta.rs/ . Adding the flag-passthrough item from 2026-08-03 will regenerate every argv expectation at once, which is exactly the case this pays for.
  Touches: `crates/terminalai-core/tests/launch_golden.rs`, `grid_ref.rs`, `Cargo.toml`
  Acceptance: argv and grid expectations are `.snap` files reviewed with `cargo insta`; snapshots are committed with `eol=lf` so CRLF does not churn them.
  Complexity: S

- [ ] P3 — Mutation-test the core modules on changed lines
  Why: a large green suite says code was executed, not that it was asserted on — and the arithmetic most likely to be executed without assertion here is backoff, cap comparisons and error-branch returns.
  Evidence: `cargo-mutants` is Windows CI-tested and supports `--in-diff` to restrict mutants to changed lines — https://mutants.rs/ . The comparison-boundary risk is concrete: `read_frame` (`crates/terminalai-daemon/src/lib.rs:132-144`) turns on `read > MAX_FRAME_BYTES` against a `take(MAX + 1)`, and the restart cap on `>=`.
  Touches: development workflow; optionally a `mutants.toml`
  Acceptance: `cargo mutants --in-diff` runs on core modules as a periodic check with PTY and process-spawning modules excluded; surviving mutants are either killed with a test or recorded as intentional. A full-tree run is explicitly not the goal.
  Complexity: S

- [ ] P3 — Embed a dependency manifest in the shipped binaries and publish an SBOM
  Why: there is currently no way to answer "which crate versions are in this exe" from the artifact itself, which is what an advisory response actually needs.
  Evidence: `cargo auditable` embeds a zlib-compressed dependency list in a `.dep-v0` linker section, under 4 kB at 400+ dependencies, with no timestamps or paths so it stays reproducibility-safe, and Windows is supported — https://github.com/rust-secure-code/cargo-auditable ; `cargo audit bin` then reads it exactly. CycloneDX is at 1.7 (ECMA-424) — https://cyclonedx.org/specification/overview/ . A locally built artifact tops out at SLSA Build L1 by definition, since L2 requires a hosted build platform — https://slsa.dev/spec/v1.2/
  Touches: the release build step, `scripts/`
  Acceptance: released binaries are built with `cargo auditable` and `cargo audit bin` resolves the full tree from the artifact; a CycloneDX SBOM ships as a release asset. If provenance is published, it states SLSA L1 rather than implying more.
  Complexity: M

- [ ] P3 — Make the sidecar and app binaries reproducible
  Why: reproducibility is the only claim that lets someone else confirm a released unsigned binary came from this source, and most of the work is configuration rather than code.
  Evidence: sources of non-determinism and their fixes are documented at https://reproducible-builds.org/docs/rust/ — `--locked`, `SOURCE_DATE_EPOCH` for anything reading the clock, and `trim-paths` (RFC 3127, default `object` in release) for absolute paths in debuginfo. The installers are out of scope and should be stated so: Tauri's WiX template generates `ProductCode Id="*"` per build and NSIS output is LZMA-solid-compressed, so the MSI cannot be byte-reproducible as templated.
  Touches: `Cargo.toml` profiles, the release build script, `README.md`
  Acceptance: two clean builds of `terminalai-daemon.exe` and `terminalai-probe.exe` from the same commit produce identical hashes on this machine, and the README states that the exes are reproducible while the installers are not.
  Complexity: M

- [ ] P3 — Say what an unsigned install actually looks like
  Why: users meet SmartScreen and possibly Smart App Control with no guidance, and the project's own README is the only place that can set the expectation honestly.
  Evidence: Microsoft documents that unsigned reputation is per file hash, "cannot transfer from previous versions unless both were signed using the same publisher identity", and builds only through download volume over weeks — resetting every release: https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation . Self-signed is rated identical to unsigned: https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options . Smart App Control blocks unsigned executables outright and applies to all executables, not just downloaded ones — so it can block `terminalai-daemon.exe` after a successful install.
  Touches: `README.md`
  Acceptance: the README states plainly that builds are unsigned, what the SmartScreen prompt looks like and how to proceed, that Smart App Control may block the daemon and how to tell, and how to verify the download instead. No claim is made that reputation will improve over time.
  Complexity: S

## Research-Driven Additions — 2026-08-06

Third external research pass (see `RESEARCH.md`), against `652f33d` / v0.9.0 with a green baseline
of 520 Rust and 300 frontend tests. It covers ground the first two did not: vendor value-set drift
since v0.6.0, the child-environment contract, release hygiene, and a competitive field that changed
shape when Anthropic shipped a parallel-agent desktop app and herdr passed 25k stars. Nothing here
duplicates an item above; where the two touch, the note says so. No numeric IDs — this file dropped
the historical `R-NN` scheme and the entries below follow the current convention.

### P0

### P1

- [ ] P1 — Tag and publish the releases that already exist
  Why: nine versions have shipped with zero git tags and zero GitHub releases, and both externally-gated distribution items say they unblock "the moment a release is published" — so the blocker on them is this repository, not a third party.
  Evidence: `git tag` returns nothing; `gh release list -R SysAdminDoc/TerminalAI` returns nothing. `Roadmap_Blocked.md` records the winget manifest as blocked because "submission needs a published GitHub Release whose asset URLs and SHA256 are already fixed. Neither exists to point at yet", and the checksum/minisign item as needing a release step to attach to. The NSIS sidecar-lock fix that the same file says must ship first landed in v0.7.0 (`f64a037`).
  Touches: `CHANGELOG.md`, a release script under `scripts/`, the built NSIS and MSI bundles
  Acceptance: the current version is tagged, an unsigned NSIS and MSI pair from a clean `cargo tauri build` is attached to a published GitHub Release, and `scripts/verify-installer.ps1` has been run against the exact uploaded artifact — including the upgrade-over-a-running-daemon path — before publishing. Ordering with the P0 changelog gate is deliberate: that gate runs first.
  Complexity: M

- [ ] P1 — Make agent identity data instead of code
  Why: adding a third agent family currently means editing a Rust enum, a hand-written flag table, the capability probe and the preset UI, which is why the ACP item is parked on "when a third agent family is on the roadmap" — the cost is the blocker, not the transport.
  Evidence: `crates/terminalai-core/src/agent.rs` hard-codes two families and `launch.rs` maps 18 flags per family by hand. herdr solves the same problem with reloadable manifests — `server.agent_manifests` and `server.reload_agent_manifests` on its socket API (https://herdr.dev/docs/socket-api/) — and covers Claude Code, Codex, Cursor, opencode, Grok, Pi, Kilo, Kimi, Antigravity, Mastra, Devin, Droid and Qoder. `Roadmap_Blocked.md` records ACP as unblocking "when a third agent family is on the roadmap"; this is the half that pays off with the two already supported.
  Touches: `crates/terminalai-core/src/agent.rs`, `launch.rs`, `capabilities.rs`, `crates/terminalai-core/tests/launch_golden.rs`
  Acceptance: resolution, flag mapping and capability probing for the existing two agents are expressed as data with the same golden argv output, byte for byte, as today; a manifest that asks for a flag the model does not express is refused at load with the field named, in the same spirit as `.terminalai/templates.toml`'s `deny_unknown_fields`. Manifests are trusted local configuration, never repository-supplied — a repo-declared agent definition would be arbitrary argv from a clone.
  Complexity: L

### P2

- [ ] P2 — Save and restore a fleet working set
  Why: a preset configures one session, so an operator restarting the machine rebuilds a twelve-session spread by hand; this is the most-requested missing feature in the two largest competitors and the leader already exposes it as an API.
  Evidence: `crates/terminalai-app/src/preset.rs:32-44` — a `Preset` is one `LaunchSpec` plus a name and description. cmux#480 "Persistence of tab information and pane layouts" (50👍) and cmux#2086 "Save and restore named workspace sessions (tmux-resurrect style)" (35👍) are its second and sixth most-reacted open issues; herdr exposes `layout.export` and `layout.apply`. The daemon already persists a versioned session store and rehydrates rings from the scrollback log, so the durable half exists.
  Touches: `crates/terminalai-app/src/preset.rs`, `crates/terminalai-core/src/store.rs`, `crates/terminalai-daemon/src/lib.rs`, `web/src/main.js`
  Acceptance: a named working set records the launch spec, project folder, pin state and grouping for many sessions and can be relaunched as one action; restoring it obeys every existing refusal — admission, memory budget, spend ceiling, dirty tree — rather than bypassing them, and reports per session which ones it declined to start and why. Restoring never adopts an existing worktree or branch, for the reason the worktree feature already refuses to.
  Complexity: M

- [ ] P2 — Archive a session when its work has landed
  Why: the archive exists and is bounded, but only the operator ever fills it, so a fleet accumulates finished rows that still read as live work.
  Evidence: `Request::Archive` (`crates/terminalai-daemon/src/lib.rs:1577`) is operator-driven and `crates/terminalai-core/src/land.rs` never calls it; the bounded archive shipped in `1658e75`. Anthropic's redesigned desktop app does the opposite by default: "When a session's PR merges or closes, it archives itself so the sidebar stays focused on what's live" (https://claude.com/blog/claude-code-desktop-redesign).
  Touches: `crates/terminalai-core/src/land.rs`, `registry.rs`, `crates/terminalai-daemon/src/lib.rs`, `web/src/main.js`
  Acceptance: a successful landing offers to archive the session it landed and records the landing in the archive entry, so the survey of leftover checkouts can tell "finished and landed" from "abandoned". A refused or partial landing archives nothing. Auto-archive is opt-in and never removes a worktree holding unmerged commits — the existing worktree rule wins.
  Complexity: M

- [ ] P2 — Decompose `registry.rs`
  Why: it is 6,106 lines and the highest-churn Rust file in the tree, it grew 34% in the two days since the last research pass measured it at 4,545, and two already-filed items are explicitly waiting on it.
  Evidence: `wc -l crates/terminalai-core/src/registry.rs` = 6,106; 62 of the last 300 commits touch it, ~1.3x the next Rust file. The "Model-based tests over the registry and session store" item names it as the surface under test, and `RESEARCH.md` rejected `loom`/`shuttle` with "Revisit if `registry.rs` is decomposed for other reasons — the extraction of admission and restart policy would get most of the way there." The natural seams are already named in that rejection: admission, restart policy, and the time-dependent state machines the mock-clock item targets.
  Touches: `crates/terminalai-core/src/registry.rs`, new modules under `crates/terminalai-core/src/`
  Acceptance: admission and restart policy are separate modules with no `Mutex`, thread spawn or `Instant` call of their own — they take decisions as pure functions over state passed in — and `registry.rs` is under 3,000 lines. Behaviour is unchanged, proven by the existing suite passing without edits to assertions. Do this before the model-based-test item, not after: the reference model is far cheaper to write against extracted policy.
  Complexity: L

- [ ] P2 — Decide whether status can come from the session's own output instead of the operator's settings file
  Why: this tool writes managed hooks into the user's global Claude and Codex configuration to learn what its own child processes are doing, and Claude Code now emits the same lifecycle events on the session's stdout — which would make the fleet observable without touching anything outside the process it launched.
  Evidence: `--include-hook-events` "include hook lifecycle events from every hook in output stream; requires `--output-format stream-json`", alongside `--forward-subagent-text` (emits subagent text with `parent_tool_use_id`) and `--include-partial-messages` (https://code.claude.com/docs/en/cli-reference). Today `crates/terminalai-core/src/hook_config.rs` (1,328 lines) installs and owns sixteen managed hook entries and has to reason about `disableAllHooks`, `allowManagedHooksOnly` and the whole HKLM/managed-settings policy chain to know whether they will fire at all.
  Touches: `crates/terminalai-core/src/hook_config.rs`, `hooks.rs`, `pty.rs`, `crates/terminalai-daemon/src/lib.rs`
  Acceptance: RESEARCH.md open question 3 is answered by one live run — whether those events match the installed-hook set and whether the flags work outside `--print` mode — and the answer is recorded in `CLAUDE.md` either way. If they match, stdout becomes the preferred channel and managed hooks become the fallback for agents that lack it, with the policy preflight kept for that fallback. If they do not, record what is missing so this is not re-investigated.
  Complexity: L

- [ ] P2 — Let one agent wait on another through the fleet surface
  Why: the fleet already knows which sessions are blocked, and the single primitive that turned the leader's API into an ecosystem is the ability for an agent to block until another one genuinely needs input; without it, an agent coordinating work polls or guesses.
  Evidence: herdr exposes `agent.wait`, `events.subscribe` and `events.wait` over newline-delimited JSON on a local socket, and describes the CLI and socket API as "the same surface agents drive: spawn panes, prompt each other, wait until another agent is genuinely blocked" (https://herdr.dev/docs/socket-api/). This project's MCP server (`crates/terminalai-core/src/mcp.rs`) is request/response only, with no event stream and no wait. Note that herdr's socket has **no authentication** — this project's pipe DACL and MCP write-token model must not be relaxed to match it.
  Touches: `crates/terminalai-core/src/mcp.rs`, `crates/terminalai-daemon/src/lib.rs`, `crates/terminalai-core/tests/mcp_server.rs`
  Acceptance: a client can subscribe to fleet status transitions and can block until a named session reaches a given state or a timeout expires, under the existing read-only-by-default rule — waiting is a read, so it needs no write token, and a wait never wakes a session or answers a prompt. Land this together with the already-filed "Move the MCP server to the current protocol revision" item rather than on top of it: the 2026-07-28 revision replaces server-initiated elicitation with Multi Round-Trip Requests, which is the mechanism this needs.
  Complexity: L

- [ ] P2 — Drive the reworked chrome in a real browser
  Why: the two most recent commits cut the always-visible chrome from 21 controls to 9 and folded seventeen launcher fields behind a disclosure, and both are covered only by jsdom assertions against `index.html` — the exact class of change that the un-minified-CSS commit says "review could not catch".
  Evidence: `d0dacb0` and `652f33d`; `web/tests/` is `node --test` over jsdom throughout, and the WebDriver path is blocked in `Roadmap_Blocked.md` under R-56 by an unretested `DevToolsActivePort` failure. The route around it is already recorded and is not blocked: the 2026-08-03 audit served the frontend with `npx vite --port 5199` and drove it with headless Chromium at both `prefers-color-scheme` values and 1440px and 1100px, computing contrast from composited `getComputedStyle` values.
  Touches: `web/scripts/`, `web/tests/`, `web/package.json`
  Acceptance: a headless script opens every dialog, both overflow menus and the launcher disclosure at 1440px and 1100px in both colour schemes, asserts no element overflows its container and no computed contrast falls below the thresholds the existing contrast test uses, and fails on regression. It is a separate script from the blocked WebDriver suite and does not depend on the virtual display.
  Complexity: M

- [ ] P2 — Say what an agent is and is not sandboxed by on Windows
  Why: the launcher's Sandbox column is empty for Claude because there is nothing to map, but an operator reading it cannot tell "this agent has no sandbox flag" from "this agent has no sandbox on this platform" — and the difference decides whether the bypass preset is reckless.
  Evidence: Claude Code's Bash sandbox "runs on macOS, Linux, and WSL2. Native Windows is not supported. On Windows, run Claude Code inside a WSL2 distribution" (https://code.claude.com/docs/en/sandboxing). `README.md:341` shows an em dash in the Claude Sandbox row without saying why. The built-in bypass preset is already deliberately paired with worktree isolation (`crates/terminalai-app/src/preset.rs:93-96`), which is the correct mitigation and is currently undocumented as one.
  Touches: `README.md`, `crates/terminalai-app/src/preset.rs`, `web/src/i18n/terminalai.ftl`
  Acceptance: the README states that on native Windows neither agent has a first-party filesystem sandbox available, that Codex's `--sandbox` is the only sandbox flag in the table and what it does and does not contain, and that the worktree plus the environment lease are what isolate a bypass session here. The bypass preset's own description says the same in one sentence.
  Complexity: S

### P3

- [ ] P3 — Decide whether the launcher should still force Claude off plan mode
  Why: choosing Claude silently rewrites a plan-mode selection to "ask", but Claude Code supports `--permission-mode plan` and this project's own mapping table says so — the reset looks like it outlived whatever made it true.
  Evidence: `web/src/main.js`'s `syncAgentFields` runs `if (!codex && $("permission-input").value === "plan") setPermissionValue("ask")`. It has been there unchanged since `1097e61`, the first Tauri shell commit, and no test covers it — `git log -S` finds only that commit. Meanwhile `launch.rs` maps `Permission::Plan` to `--permission-mode plan` for Claude and to `collaboration_mode.mode="Plan"` for Codex, so both agents express it, and `README.md`'s table lists Plan for both. Two built-in Claude presets ("Claude · Plan first", "Claude · Quick question") carry `Permission::Plan` and are therefore rewritten the moment the launcher syncs its fields.
  Touches: `web/src/main.js`, `web/tests/launcherSafety.test.mjs`
  Acceptance: either the reset is removed and a test asserts a Claude session keeps plan mode from a preset through to the previewed argv, or the reset is kept with a comment naming the Claude behaviour that requires it. Verify against a real `claude --permission-mode plan` launch before removing — the flag being documented is not proof this build accepts it.
  Complexity: S

- [ ] P3 — Tell the agent when the operator is using a screen reader
  Why: the app has an explicit opt-in screen-reader mode for its own terminal, and the agent whose output fills that terminal has a matching mode that is never turned on, so the accessible surface stops at the renderer.
  Evidence: the focused terminal toolbar's screen-reader opt-in is described in `README.md:132-133`. Claude Code exposes `--ax-screen-reader`, "render screen-reader friendly output; flat text without decorations (v2.1.181+)", and the environment variable `CLAUDE_AX_SCREEN_READER` (https://code.claude.com/docs/en/cli-reference, /settings). Neither is reachable: the flag is not in `launch.rs`'s table and the variable is not in `safe_environment_keys()`.
  Touches: `crates/terminalai-core/src/launch.rs`, `environment.rs`, `web/src/main.js`
  Acceptance: turning on the app's screen-reader mode launches new sessions with the agent's equivalent where the agent has one, and says plainly that it cannot change sessions already running. An agent with no equivalent is not silently launched as if it had one. Pairs with the launcher-flag passthrough item above; file the flag there if that item lands first.
  Complexity: S

- [ ] P3 — Attribute the quota window to the sessions that consumed it
  Why: the header already reports that a provider is rate limiting and when the window reopens, but not which of the running sessions spent it — and subscription-window exhaustion is the single loudest operational complaint about the agents this tool supervises.
  Evidence: anthropics/claude-code#16157 "[BUG] Instantly hitting usage limits with Max subscription" (723👍) and #38335 "[BUG] Claude Max plan session limits exhausted abnormally fast" (539👍); a Sculptor HN commenter states plainly that "Claude Code runs into limits below the $200 tier" (https://news.ycombinator.com/item?id=45427697). The ledger already exists — `crates/terminalai-core/src/spend.rs` keeps a rolling window persisted with the session store — and the rollup already breaks spend down by agent, folder and session, so the data is present and the window view is not.
  Touches: `crates/terminalai-core/src/spend.rs`, `web/src/rollup.js`, `web/src/rateLimit.js`, `web/src/main.js`
  Acceptance: when a provider reports rate limiting, the fleet can show which sessions consumed the current window and in what proportion, sourced from the same transcript arithmetic the rollup already uses, with unpriced sessions counted apart rather than as zero. It never presents an estimate as the provider's own accounting — the price-table tooltip's existing wording is the model.
  Complexity: M

- [ ] P3 — Build for Windows on ARM
  Why: the project's entire differentiator is Windows, and it ships x86_64 only, so every Snapdragon-class Windows machine runs the whole supervised fleet — daemon, probe and both agents — under emulation.
  Evidence: `crates/terminalai-app/tauri.conf.json` declares no architecture and the bundle is produced for the host only; `scripts/check-cross-targets.ps1` defaults to `x86_64-unknown-linux-gnu` and its package list excludes `terminalai-app`. Nothing in the tree references `aarch64-pc-windows-msvc`. The Windows-specific code is `windows-sys` calls (job objects, EcoQoS, ConPTY, named pipes, taskbar) rather than intrinsics, so the port is a target and a bundle question rather than a code one — **Likely**, not verified by a build.
  Touches: `scripts/check-cross-targets.ps1`, the release build step, `README.md`
  Acceptance: `aarch64-pc-windows-msvc` is at least type-checked by the cross-target script alongside the Linux target, and the README states which architectures a release actually ships. If an ARM64 bundle is produced, it goes through `scripts/verify-installer.ps1` on real hardware before being called supported — an untested second architecture is worse than an honest single one.
  Complexity: M

- [ ] P3 — Publish the design reasoning the repository currently keeps to itself
  Why: this is a public MIT repository whose every non-obvious decision — the 28px row, push-not-poll, unwind-not-abort, the 34.8 µs containment window, refuse-do-not-drop — lives only in a gitignored file, so a reader sees the conclusions and none of the arguments.
  Evidence: `.gitignore` excludes `CLAUDE.md`, which holds the Design decisions and Learned sections; there is no `docs/` directory and no architecture document in the tree. `README.md` is 396 lines and already carries some of this reasoning inline, which is why it is that long.
  Touches: `docs/`, `README.md`
  Acceptance: the design decisions and the platform traps worth publishing are moved into a tracked `docs/` file that the README links to, and the README shortens by what moved. `CLAUDE.md` keeps the working notes and the `## Learned` log; nothing about an AI author or agent workflow is copied across. Pairs with the already-filed `.github/` surface item — a bug template is more useful next to a document explaining what the daemon is.
  Complexity: S
