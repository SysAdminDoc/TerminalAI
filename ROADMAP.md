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

- [ ] P1 — A settings surface for the knobs that currently require an environment variable and a daemon restart
  Why: the product thesis is thirty rows and the shipped default admits three, changeable only by setting `TERMINALAI_MAX_LIVE_SESSIONS` before the daemon starts — a default that contradicts the pitch and a control no user will find.
  Evidence: `DEFAULT_MAX_LIVE_SESSIONS = 3` at `crates/terminalai-core/src/registry.rs:42`; `AdmissionConfig::from_environment` at `:79-107`. `web/index.html` defines six dialogs (launcher, broadcast, projects, queue, rollup, explainer) and no settings dialog; grep for "settings" in that file returns nothing. Distinct from the existing P2 about removing presets and project roots — this is daemon-wide policy, not stored-item CRUD.
  Touches: `web/index.html`, `web/src/main.js`, `crates/terminalai-app/src/main.rs`, `crates/terminalai-daemon/src/lib.rs`, `crates/terminalai-core/src/registry.rs`
  Acceptance: admission cap, default session budget, notification thresholds and the new spend ceiling are editable in-app and applied without restarting the daemon; environment variables remain the boot default and the dialog says when a value came from one.
  Complexity: M

### P2

- [ ] P2 — Cap the archive vector; it grows forever inside a file rewritten every second
  Why: every archived session is appended to an unbounded `Vec` that is serialized into each full store snapshot, so persistence cost rises monotonically for the life of the install on a hot path.
  Evidence: `crates/terminalai-core/src/registry.rs:994` (`state.archives.push`) with no cap or truncation anywhere; `crates/terminalai-core/src/store.rs:26` includes `archives` in `StoreSnapshot`. `README.md` documents persistence after a 200 ms quiet period and at least once per second under sustained output.
  Touches: `crates/terminalai-core/src/registry.rs`, `crates/terminalai-core/src/store.rs`, `crates/terminalai-daemon/src/persistence.rs`
  Acceptance: archives are bounded by count and age with the limit stated in one constant; trimming is covered by a test that writes past the bound. Consider a sidecar file so archive growth cannot affect live-session write latency at all.
  Complexity: S

- [ ] P2 — Close the spawn-to-job race so a grandchild cannot escape containment
  Why: the job is assigned after the process exists, so anything the child spawns in that window is outside the kill-on-close guarantee the whole teardown story rests on.
  Evidence: `ProcessJob::assign()` (`crates/terminalai-core/src/process_tree.rs:32-57`) takes an already-created `RawHandle`; `portable-pty` performs the `CreateProcess`. The documented fix is `PROC_THREAD_ATTRIBUTE_JOB_LIST` via `UpdateProcThreadAttribute` at creation — learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute
  Touches: `crates/terminalai-core/src/process_tree.rs`, `crates/terminalai-core/src/pty.rs`, possibly a `[patch.crates-io]` fork of `portable-pty`
  Acceptance: the agent process is created already inside its job, or — if reaching into `portable-pty`'s spawn proves impractical — the residual window is measured, documented, and the decision recorded rather than left implicit. Nested jobs give the fleet-wide second tier if adopted.
  Complexity: M

- [ ] P2 — Un-minify `styles.css` and finish the frontend module extraction
  Why: the P1 "a literal `\n` inside styles.css kills the Review view's only layout" was fixed on 2026-08-04, but its cause was not — a 2,204-character line is a file where that class of defect is undetectable by review, so it will recur.
  Evidence: `web/src/styles.css` still has 29 lines over 300 characters, longest 2,204 after that fix (measured with `awk '{print length}' | sort -rn`). `web/src/main.js` is 2,800 lines / 119 KB with a longest line of 1,348, beside five already-extracted modules (`broadcast.js`, `fleetRows.js`, `projects.js`, `rateLimit.js`, `rollup.js`) that establish the pattern.
  Touches: `web/src/styles.css`, `web/src/main.js`, new modules under `web/src/`
  Acceptance: no line in `web/src/` exceeds a stated column limit, enforced by a test in the same spirit as `rowDensity.test.mjs`; the fix for the P1 above lands in a file where the next such typo is visible in a diff.
  Complexity: M

- [ ] P2 — Surface the session history the store already keeps
  Why: `ArchivedSession` records id, agent, name, cwd and the exact command, persists across restarts, and is read back only to advance a counter — a finished-work view exists as data with no renderer.
  Evidence: `crates/terminalai-core/src/store.rs:60-66` defines the record; `registry.rs:435-439` reads archives solely to compute `next_id`; no Tauri command in `crates/terminalai-app/src/main.rs` exposes them (compare the roughly 55 registered commands).
  Touches: `crates/terminalai-daemon/src/lib.rs`, `crates/terminalai-app/src/main.rs`, `crates/terminalai-app/capabilities/default.json`, `web/src/main.js`
  Acceptance: a history view lists archived sessions with their command and folder and can relaunch one into the launcher pre-filled. Pairs naturally with the archive cap above — bound it before exposing it.
  Complexity: M

- [ ] P2 — An approvals inbox across the fleet
  Why: permission decisions are the fleet's blocking work and are currently answered one focused session at a time; the field has converged on a single queue, and Claude Code exposes the hook needed to answer programmatically.
  Evidence: `SessionStatus::NeedsApproval` exists (`crates/terminalai-core/src/session.rs:73`) but no aggregate surface does. `PermissionRequest`/`PermissionDenied` are already managed in `hook_config.rs` and support `updatedInput` and `retry` (code.claude.com/docs/en/hooks). Requested at anthropics/claude-code#58247; shipped by wmux and octomux (https://github.com/ShreyPaharia/octomux).
  Touches: `crates/terminalai-core/src/hooks.rs`, `crates/terminalai-core/src/registry.rs`, `crates/terminalai-daemon/src/lib.rs`, `web/index.html`, `web/src/main.js`
  Acceptance: one dialog lists every pending permission request across sessions with its tool and arguments, and answering routes to the right session. It never auto-approves and never enables bypass mode on the operator's behalf — cmux was criticised for exactly that (https://github.com/manaflow-ai/cmux/issues/3547).
  Complexity: L

- [ ] P2 — Show context pressure, and distinguish context exhaustion from refusal
  Why: compaction and context loss are a top-tier community complaint, and a session approaching its window is about to lose quality in a way no current row state predicts.
  Evidence: Claude Code's statusline payload carries `context_window.used_percentage`, `remaining_percentage` and `exceeds_200k_tokens` (code.claude.com/docs/en/statusline); Codex config exposes `model_context_window` and `model_auto_compact_token_limit`. Nothing in `crates/terminalai-core/src/session.rs` models context. Asked directly at https://github.com/Untrivial-ai/agent-orchestrator/issues/3322; community evidence at openai/codex#4106, #11325.
  Touches: `crates/terminalai-core/src/transcript.rs`, `session.rs`, `web/src/main.js`, `web/src/i18n/terminalai.ftl`
  Acceptance: the wide row shows context used against the model's window when the agent reports it, an em dash when it does not, and a compaction event is visible in the status history rather than appearing as an unexplained pause.
  Complexity: M

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

- [ ] P2 — Report and reap stale and fully-merged session worktrees
  Why: teardown deliberately keeps a branch holding unmerged work, which is correct, but nothing ever revisits it — so worktrees and branches accumulate silently and their registrations outlive the directories.
  Evidence: `crates/terminalai-core/src/registry.rs:973-978` releases the worktree and logs failures without follow-up; `crates/terminalai-core/src/worktree.rs` has no sweep. The same accumulation is filed against competitors: https://github.com/superset-sh/superset/issues/2863, https://github.com/getpaseo/paseo/issues/1227, and branch cleanup is asked for at https://github.com/Untrivial-ai/agent-orchestrator/issues/3411
  Touches: `crates/terminalai-core/src/worktree.rs`, `crates/terminalai-core/src/registry.rs`, `crates/terminalai-app/src/projects.rs`, `web/src/main.js`
  Acceptance: a view lists worktrees this tool created that no live session owns, marking each merged or unmerged; removing a merged one prunes the registration too. Nothing with unmerged commits is ever removed without an explicit, individually confirmed action.
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

- [ ] P2 — 24 `cfg(unix)` code paths have never been compiled
  Why: no non-Windows target is installed, so the cross-platform branches are not merely untested — they have never been type-checked, and nothing would catch a syntax or signature error in them.
  Evidence: `rustup target list --installed` returns empty and `rustup toolchain list` shows only the linked `terminalai` toolchain. 24 `cfg(unix)`/`cfg(not(windows))` occurrences across 8 files: `pty.rs`, `atomic_file.rs`, `environment.rs`, `external.rs`, `review.rs`, `terminalai-app/src/main.rs`, `terminalai-daemon/src/lib.rs`, `terminalai-probe/src/main.rs`. The roadmap's own "Unaudited — needs a pass" item names non-Windows paths as uncovered.
  Touches: `scripts/verify-installer.ps1` or a new check script; no source changes required to start
  Acceptance: `rustup target add x86_64-unknown-linux-gnu` and `cargo check --target x86_64-unknown-linux-gnu --workspace` run clean and are part of the pre-release checks. Running those branches is a separate step (WSL2); compiling them is the cheap half and closes the larger hole.
  Complexity: S

- [ ] P2 — The active Rust toolchain cannot install components, blocking coverage and Miri
  Why: `llvm-tools-preview` cannot be added to a rustup *linked* toolchain, so `cargo-llvm-cov` and `cargo miri` are unavailable — two of the cheapest ways to find untested error arms and UB in the `windows-sys` FFI glue.
  Evidence: `rustup toolchain list` shows only `terminalai (active, default)`, which is the standalone MSI install linked into rustup; `rustup component add llvm-tools-preview` fails with "toolchain 'terminalai' does not support components", and no `llvm-cov`/`llvm-profdata` exists under that toolchain's `bin`. This is also the root cause of the previously recorded `cargo tauri build` toolchain trap in `CLAUDE.md`.
  Touches: machine toolchain configuration; record the change in the machine-state notes, not in the repo
  Acceptance: a managed `stable-x86_64-pc-windows-msvc` toolchain is installed and used for tooling, `cargo llvm-cov --workspace` produces a report, and the existing build path still works. Coverage is used once to find zero-coverage error arms and `cfg` branches, not as a number to chase.
  Complexity: S

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

- [ ] P2 — Publish a winget manifest
  Why: it is the highest-leverage distribution change available and it costs nothing in policy terms — unsigned installers are explicitly accepted, and installing through winget bypasses the browser download that attaches Mark-of-the-Web in the first place.
  Evidence: signing is required only for MSIX; verbatim in the schema docs — https://github.com/microsoft/winget-pkgs/blob/master/doc/manifest/schema/1.12.0/installer.md . Unsigned-installer packages are live in the community repo (`wez.wezterm`, `yt-dlp.yt-dlp`, `Neovim.Neovim`). Current schema is 1.12.0; validation failures are AV/PUA scans, non-silent install, non-HTTPS or redirected URLs, and incomplete uninstall — never the absence of a signature: https://learn.microsoft.com/en-us/windows/package-manager/package/winget-validation
  Touches: a new release step; manifests live in the upstream `microsoft/winget-pkgs` repo, not here
  Acceptance: `winget install SysAdminDoc.TerminalAI` installs and `winget upgrade` moves versions; the manifest declares `Silent: /S` for NSIS and `/quiet` for MSI, `UpgradeBehavior`, and `AppsAndFeaturesEntries` carrying the MSI `UpgradeCode`. Depends on the NSIS sidecar fix — do not publish an installer whose upgrade path is known broken.
  Complexity: M

- [ ] P2 — Publish checksums and a detached signature with each release
  Why: nothing currently lets anyone verify a downloaded artifact is the one that was built, and a certificate-free detached signature is the only integrity mechanism available under the no-signing policy.
  Evidence: minisign is Ed25519 with no CA and no PKI — https://jedisct1.github.io/minisign/ — so it is not code signing and does not conflict with the policy. Syncthing publishes a checksum file plus an independent signature its updater verifies (https://docs.syncthing.net/dev/release-signing.html); yt-dlp publishes `SHA2-256SUMS` plus GPG signatures. Of five unsigned Windows projects surveyed, two publish nothing usable.
  Touches: the release process and `scripts/`, `README.md`
  Acceptance: every release carries `SHA256SUMS` and a detached signature over it, the public key is in the README, and a `verify-release.ps1` checks both. Note honestly that this protects the transport only — per Microsoft's own documentation it changes nothing about SmartScreen or Smart App Control.
  Complexity: S

### P3

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
