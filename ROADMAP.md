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

## Research-Driven Additions — 2026-08-08

Filed from the research pass in `RESEARCH.md` (2026-08-08), against v0.21.0 with a green baseline.
Nothing here re-files an item that already exists above or in `Roadmap_Blocked.md`. One item is
deliberately about *unblocking* two entries in that file and names them rather than copying them;
where an item depends on another in this section, its Why says which.

### P0

### P1

- [ ] P1 — Run `verify-goldens` in the release gate, and teach it what a flag's mode means
  Why: the check that asks whether the installed agent accepts our argv is wired to nothing, and it could not have caught the budget defect above because it compares flag names against `--help` without reading the constraints `--help` states next to them.
  Evidence: `terminalai-probe verify-goldens` is documented (its own `USAGE` at `crates/terminalai-probe/src/main.rs:59`, and `CLAUDE.md:69`) but invoked by nothing — no file under `scripts/` or `web/scripts/`, no `package.json` script, no cargo alias, and `.github/` contains no workflows. `claude --help` prints the constraint inline — "(only works with --print)" — but on a wrapped continuation line, and `help_lists_flag` (`crates/terminalai-core/src/help.rs:85`) searches the whole help string for a token with no per-flag block to attach a constraint to, so the second half of this item is a parser rewrite rather than a predicate tweak.
  Touches: `crates/terminalai-probe/src/main.rs`, `crates/terminalai-core/src/help.rs`, `scripts/verify-release-metadata.ps1` or a new gate step it invokes
  Acceptance: a release run fails when the installed agent does not accept a golden argv, and fails when a golden emits a flag whose own help text restricts it to a mode this tool does not use. Both failures are reproducible by hand-editing a fixture.
  Complexity: M (wiring is small; the flag-mode half is a help-text parser)

- [ ] P1 — Make the README describe what ships
  Why: it claims a capability that does not exist in the code and a platform range the bundle cannot produce, and a README that overstates is the cheapest kind of trust to lose.
  Evidence: `README.md:37` — "The daemon can hibernate idle sessions while retaining their rows" — while hibernation is unimplemented (no match for `hibernat` anywhere outside `README.md` and `Roadmap_Blocked.md`) and parked as R-06 pending live validation. `README.md:5` badges `platform-Windows | macOS | Linux` while `tauri.conf.json` bundles `nsis`/`msi` only, `deny.toml` graphs `x86_64-pc-windows-msvc` only, the app crate takes `windows-sys` unconditionally, and `scripts/check-cross-targets.ps1` excludes the app crate by design.
  Touches: `README.md`
  Acceptance: hibernation is described as planned or removed — note that an adjacent mechanism does ship, `set_background_priority` demoting CPU and memory priority on focus change, so the honest fix is a rewrite rather than a deletion — the badge states the platform that is actually bundled, and the non-Windows story is stated once as "core, daemon and probe type-check; the shell is Windows-native".
  Complexity: S

- [ ] P1 — Re-verify against Claude Code 2.1.226 and land the two version-gated items
  Why: both items in `Roadmap_Blocked.md` were blocked on flags that postdated the installed CLI, and the versions that carry them are now published — so the blocker is an upgrade decision rather than an absence.
  Evidence: https://registry.npmjs.org/@anthropic-ai/claude-code/latest reports **2.1.226** (checked 2026-08-08); this machine runs 2.1.170. `--ax-screen-reader` appears in the changelog at 2.1.208 and is documented from 2.1.181; `--autocompact <auto|tokens>` at 2.1.221. Cross-reference rather than duplicate: "Tell the agent when the operator is using a screen reader" and "Let the operator set the compaction threshold the row now displays" already exist in `Roadmap_Blocked.md` — move them back, do not re-file them.
  Touches: `Roadmap_Blocked.md`, `crates/terminalai-core/tests/fixtures/launch/` (a fixture for the new version). The two moved items carry their own Touches; do not restate them here.
  Acceptance: the operator has upgraded (their call — do not upgrade the CLI they are running mid-session), `terminalai-probe verify-goldens` passes against the new version, a golden fixture for it exists beside the 2.1.170 one, and the two blocked entries are moved into this file unchanged apart from their blocker note. The compaction item is re-read first: it models itself on `max_budget_usd`, which the P0 above removes or relabels, and `--autocompact` needs its own mode check before it is mapped.
  Complexity: M

- [ ] P1 — Say how many agents a row really is
  Why: since agent teams, one supervised session can be a lead plus N separate Claude Code instances, and the fleet shows it as one row with one status — the density thesis only holds if a row's cost is legible.
  Evidence: https://code.claude.com/docs/en/agent-teams — the team config lives at `~/.claude/teams/{team-name}/config.json` with a `members` array of name and agent id, the team name is `session-` plus the first eight characters of the session id, and the directory is removed when the session ends. The id to derive the path from is `spec.session_id` (`crates/terminalai-core/src/launch.rs:243`), which this tool assigns at launch — not `session.resume_id`, which is populated later from an ingested hook (`registry/ingest.rs:187`) and is absent until the session reports one. `SubagentStart`/`SubagentStop` are already managed (`hook_config.rs`) but only flip Working/Thinking (`registry/ingest.rs:242`, `:250`). No competitor surveyed reports team composition per session.
  Touches: `crates/terminalai-core/src/external.rs` or a sibling reader, `crates/terminalai-core/src/session.rs`, `crates/terminalai-core/src/registry/sampling.rs`, `web/src/rowMarkup.js`, `web/src/i18n/terminalai.ftl`
  Acceptance: a row that is a team lead names its teammates in the wide row; a session with no team, no assigned session id, or an unreadable team file reports nothing rather than "1" or "0"; a team directory left behind by a session that has ended is not attributed to a live row. The *count* of processes belongs to the job-memory item above — this one adds names, not a second number.
  Complexity: M

- [ ] P1 — Manage `WorktreeRemove` so the survey stops naming checkouts that are gone
  Why: the supervisor answers `WorktreeCreate` with a placement and then surveys the root for strays, but nothing tells it when the agent removed one — so a checkout the agent cleaned up can still be offered for reaping.
  Evidence: `WorktreeRemove` is a documented hook event (https://code.claude.com/docs/en/hooks) and is absent from both `CLAUDE_EVENTS` and the `DELIBERATELY_UNMANAGED` list in `crates/terminalai-core/src/hook_config.rs:48-103` — so it is unhandled rather than deliberately unhandled, which is exactly the distinction that list exists to record.
  Touches: `crates/terminalai-core/src/hook_config.rs`, `crates/terminalai-core/src/hooks.rs`, `crates/terminalai-core/src/registry/provisioning.rs`
  Acceptance: a removal reported by the agent updates the survey without a rescan, and a `WorktreeRemove` for a path this tool does not own changes nothing. If it is decided not to manage it, it joins `DELIBERATELY_UNMANAGED` with its reason instead.
  Complexity: S

### P2

- [ ] P2 — Find out whether a teammate's hooks arrive as the lead's
  Why: a teammate launched by a team lead inherits the lead's environment, including the hook token the daemon matches on — so a `Notification` or `SubagentStop` from a separate agent instance may be landing on the lead's row and moving a status that is not about it. Nothing today can tell the two apart, and the answer decides whether the notification arms added on 2026-08-08 need scoping.
  Evidence: hook matching requires `entry.session.hook_token` (`crates/terminalai-core/src/registry/ingest.rs`), which is placed in the session's environment at launch; `https://code.claude.com/docs/en/agent-teams` says teammates are separate Claude Code instances started by the lead. `agent_completed` currently sets the lead's row to `AwaitingInput` by name — the same mapping the substring match produced before — and if teammate completions arrive on that token, a lead still working would be reported as waiting.
  Touches: `crates/terminalai-core/src/registry/ingest.rs`, `crates/terminalai-core/src/hooks.rs`
  Acceptance: a real team run is observed and the finding recorded either way. If teammate hooks do arrive on the lead's token, they carry something that distinguishes them and the notification arms use it; if they do not, that is stated in the ingest module docs so the question is not reopened.
  Complexity: S

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

- [ ] P2 — Give the four state-less dialogs a way to fail
  Why: four surfaces render from state the window already holds and therefore have no loading state, which is right — but they also have no error state, so a renderer that throws leaves an empty dialog with nothing said.
  Evidence: `#explainer-dialog`, `#approvals-dialog`, `#broadcast-dialog` and `#rollup-dialog` have neither a loading nor an error path; `renderDataError(container, message, action, retry)` already exists at `web/src/main.js:285` and is used at five other sites. The search dialog catches its error but surfaces it only as a toast, leaving the body blank.
  Touches: `web/src/main.js`, `web/src/approvals.js`, `web/src/broadcast.js`, `web/src/rollup.js`, `web/src/i18n/terminalai.ftl`
  Acceptance: each of the five renders a stated failure in its own body rather than nothing, verified by a test that makes each renderer throw. Loading states are deliberately not added where the data is already in memory, and that decision is recorded in the module docs. Scope is these five; the seven other surfaces `RESEARCH.md` counts as missing a state are missing only a loading state on data that does arrive asynchronously, and are not filed.
  Complexity: S

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

- [ ] P3 — Make the hooks preflight prove itself rather than read a file
  Why: the check reports "installed" from the settings file and from managed policy, which is a claim about configuration, not evidence that a hook fires and reaches the daemon. Cannot start until the Claude Code upgrade item above lands — `--init-only` does not exist on 2.1.170.
  Evidence: the `hooks` check (`crates/terminalai-app/src/main.rs:1495`, `:1600-1650`) inspects installed state and blocking policy only. `claude --init-only` — "run Setup and SessionStart hooks, then exit without starting a conversation" (https://code.claude.com/docs/en/cli-reference) — would make the hook actually fire; it is absent from `claude --help` on 2.1.170, so this depends on the 2.1.226 upgrade item above.
  Touches: `crates/terminalai-app/src/main.rs`, `crates/terminalai-daemon/src/http_hooks.rs`
  Acceptance: the preflight check reports "installed and firing" only after the daemon observed a hook from a real `--init-only` run, distinguishes that from "installed, not yet proven", and never blocks startup on the probe failing.
  Complexity: M
