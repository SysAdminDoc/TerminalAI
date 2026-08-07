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

- [ ] P3 — Unaudited surfaces from the 2026-08-07 pass
  Category: docs
  Where: repository-wide
  Problem: this pass did not deep-audit: `scripts/verify-installer.ps1` (runs only at release, needs a `cargo tauri build` and a window), `crates/terminalai-app/src/preset.rs` and `src/work.rs` beyond their public seams, `crates/terminalai-core/src/external.rs` parsing of Claude's own session registry, the packaged-app E2E path (`run-e2e.mjs`, known-blocked on DevToolsActivePort), or populated-fleet theming — the chrome gate audits empty states, so rows with live tones, the rate-limit banner and the spend header have never been contrast-checked with real data in the light theme.
  Evidence: coverage of this audit session; the chrome gate's SURFACES list contains no populated-fleet state.
  Fix: next audit pass starts here. For populated-fleet theming specifically: extend `chrome-audit.mjs` with a fixture mode that injects a synthetic snapshot (the `renderFixtureRow` harness already builds row markup) before auditing, so row tones, the unread gradient and the header chips are contrast-checked in both themes.
  Acceptance: a later audit either clears these areas or files findings from them.
  Confidence: Verified
  Effort: M

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

- [ ] P3 — Let the operator set the compaction threshold the row now displays
  Why: the context reading landed 2026-08-07, so the fleet reports how full a window is but cannot influence when the agent acts on it — and both agents take the threshold as a launch-time input this launcher does not map.
  Evidence: Claude Code added `--autocompact <auto|tokens>` in v2.1.221 and the `CLAUDE_CODE_AUTO_COMPACT_WINDOW` variable (100000–1000000 tokens); Codex exposes `model_auto_compact_token_limit`. `LaunchSpec` (`crates/terminalai-core/src/launch.rs:203`) maps neither; `max_budget_usd` is the shape to mirror — an existing optional numeric that is Claude-only in the same way. `web/src/contextPressure.js` already has the cell that would show the threshold beside the usage.
  Touches: `crates/terminalai-core/src/launch.rs`, `environment.rs`, `web/src/main.js`, `web/src/contextPressure.js`, `crates/terminalai-core/tests/launch_golden.rs`
  Acceptance: the launcher can set a compaction threshold per session, it reaches the previewed argv for both agents, and the row shows it beside the occupancy so a session near its own threshold is visible before it compacts. An agent with no equivalent is refused rather than silently launched without it, per `LaunchError::Unsupported`.
  Complexity: S

### P3

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

### P2

- [ ] P2 — Decide whether status can come from the session's own output instead of the operator's settings file
  Why: this tool writes managed hooks into the user's global Claude and Codex configuration to learn what its own child processes are doing, and Claude Code now emits the same lifecycle events on the session's stdout — which would make the fleet observable without touching anything outside the process it launched.
  Evidence: `--include-hook-events` "include hook lifecycle events from every hook in output stream; requires `--output-format stream-json`", alongside `--forward-subagent-text` (emits subagent text with `parent_tool_use_id`) and `--include-partial-messages` (https://code.claude.com/docs/en/cli-reference). Today `crates/terminalai-core/src/hook_config.rs` (1,328 lines) installs and owns sixteen managed hook entries and has to reason about `disableAllHooks`, `allowManagedHooksOnly` and the whole HKLM/managed-settings policy chain to know whether they will fire at all.
  Touches: `crates/terminalai-core/src/hook_config.rs`, `hooks.rs`, `pty.rs`, `crates/terminalai-daemon/src/lib.rs`
  Acceptance: RESEARCH.md open question 3 is answered by one live run — whether those events match the installed-hook set and whether the flags work outside `--print` mode — and the answer is recorded in `CLAUDE.md` either way. If they match, stdout becomes the preferred channel and managed hooks become the fallback for agents that lack it, with the policy preflight kept for that fallback. If they do not, record what is missing so this is not re-investigated.
  Complexity: L

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
