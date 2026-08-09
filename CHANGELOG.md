# Changelog

All notable changes to TerminalAI are recorded here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [0.24.0] — 2026-08-08

### Added

- Added a versioned Claude/Codex compatibility matrix shared by the Rust launch goldens and
  `terminalai-probe verify-goldens`. It checks emitted argv shape, explicit unsupported choices,
  exact installed CLI versions, vendor flag coverage and mode-restricted options before a session
  can be spawned.
- Hardened the packaged Tauri/WebView2 smoke path with isolated-display placement retries, native
  daemon event delivery, focused-terminal resize/attach assertions and outage recovery screenshots.
- Added a first-run checklist with local-only progress, task-oriented empty-state guidance, and a
  read-only safe demo that covers every fleet status without starting an agent or sending daemon
  commands. The packaged WebView2 gate now drives the demo and proves focus stays offline.
- Added explicit hook attribution outcomes and daemon-lifetime delivery proof. Authenticated events
  that match multiple live rows are surfaced as ambiguous and refused instead of mutating whichever
  row happens to be first; preflight now distinguishes “installed, not yet proven” from “installed
  and firing”.
- Added the headless `terminalai-probe fleet-stress` gate and
  `scripts/verify-fleet-stress.ps1`: a deterministic 30-session profile that reports startup,
  hook/snapshot latency, CPU/RSS, bounded queues and scrollback, plus store recovery evidence.
- Added a tagged Windows release lane that installs supply-chain tooling, verifies embedded
  dependency manifests, publishes per-binary CycloneDX SBOMs, and includes their hashes in the
  release manifest alongside the unsigned NSIS/MSI installers.
- Extracted fleet status ordering, metadata and lifecycle semantics into the injectable
  `web/src/sessionStatus.js` module, added behavior tests for the real helper, and made Rust app
  source-contract tests read the complete app module set so command-family moves remain safe.
- Extracted offline first-run demo state swapping and focused-terminal header/placeholder updates
  into the injectable `web/src/sessionDemo.js` and `web/src/terminalHeader.js` modules while
  retaining the existing demo isolation and terminal layout behavior.
- Extracted daemon-driven session reconciliation, attention toasts, and coalesced accessible
  status announcements into the injectable `web/src/sessionState.js` module so snapshot replay
  and live events share one UI-state lifecycle.
- Extracted focused-terminal measurement, debounced resizing, and per-session geometry delivery
  into the injectable `web/src/terminalLayout.js` module while retaining the adaptive grid and
  default-size contracts.
- Extracted fleet row creation, keyboard movement, action dispatch, and incremental DOM updates
  into the injectable `web/src/fleetRowState.js` module; returned `rowAction` from the terminal
  history boundary so row controls no longer rely on an unbound closure.
- Extracted urgency-aware grouping, structured status/agent filters, and group-chip rendering
  into the injectable `web/src/fleetGrouping.js` module; filter contracts now read that boundary.
- Extracted fleet list reconciliation, empty/loading states, and interaction-safe priority ordering
  into the injectable `web/src/fleetList.js` module; list affordance and visual-state contracts now
  read the owning coordinator.
- Extracted snapshot refresh serialization, event replay, preflight outage routing, and focused
  terminal reattachment into the injectable `web/src/snapshotCoordinator.js` module; operational
  panels use a late-bound loader so the boundary remains acyclic.
- Extracted admission settings loading, validation, usage cards, and save/refresh behavior into
  the injectable `web/src/settingsPage.js` module; settings source contracts now follow that
  page boundary.
- Extracted fleet summary accounting and pinned grid-preview reconciliation into the injectable
  `web/src/fleetSummary.js` and `web/src/pinnedPanes.js` modules; summary, quota, and pinned-pane
  source contracts now follow their owning boundaries.
- Extracted URL validation, agent and hook readiness, daemon checks, and Start-Menu repair into
  `crates/terminalai-app/src/preflight.rs` while keeping the existing Tauri command surface and
  app-level behavior tests green.
- Extracted daemon event fan-out, buffered output delivery, taskbar progress and waiting badges,
  toast activation, and scheduled-work polling into `crates/terminalai-app/src/events.rs`, leaving
  the Tauri entry module responsible for startup and command registration.
- Moved the browser event coordinator into `web/src/eventBindings.js`, made workspace-page handlers
  explicit at their factory boundary, and covered the returned approval/project handlers with a
  module-level test.
- Extracted review rendering, loading, landing, and review-state actions into the injectable
  `web/src/reviewPage.js` module; migrated review and cost source-contract assertions to the
  module boundary so future shell moves keep testing the behavior they cover.
- Extracted the read-only external-session panel into `web/src/externalSessions.js`; its source
  assertions now follow the module and continue to enforce failed-lookup, agent-reported-state,
  and non-actionable-row behavior.
- Extracted broadcast rendering, live selection synchronization, and refusal-aware sending into
  `web/src/broadcastPanel.js`; the eligibility and protocol tests now follow the panel boundary
  while event wiring remains in the shell coordinator.
- Extracted the spend rollup tables, coverage/window breakdown, and guarded dialog entry into
  `web/src/rollupPage.js`; rollup source assertions now read the owning page module.
- Extracted the row-model explainer and its guarded page entry into `web/src/explainerPage.js`,
  keeping the explainer's status vocabulary sourced from the shared fleet metadata.
- Extracted serialized focused-session switching, stale-failure guards, and route restoration into
  `web/src/sessionFocus.js`; workspace-page creation now consumes the coordinator after terminal
  history is initialized.
- Split the stylesheet into import-ordered `tokens`, `foundation`, `pages`, and `shell` layers;
  frontend style assertions now resolve the same assembled cascade through `web/tests/cssSource.mjs`.
- Added the tagged Windows release lane in `.github/workflows/release.yml`: Linux/macOS cfg tests,
  Windows cross-target checks, clean-build executable reproducibility, isolated installer upgrade
  evidence, SHA-256 release manifests, explicit unsigned policy, and generated Winget metadata.
- Added `scripts/prepare-release-assets.ps1`, which derives the exact versioned NSIS/MSI artifacts,
  hashes, commit provenance, and MSI ProductCode/UpgradeCode from the built release instead of
  trusting filenames or a hand-maintained package manifest.
- Stabilized the release test gate under Windows load: the 30-session synthetic profile now backs
  off while polling its shared registry lock, branch semantics retry bounded Git lookups in tests,
  and the ConPTY ETX contract accepts both current Windows control-event behavior and the older
  byte-only behavior. Its whole-workspace startup budget is 60 seconds so the heavier opt-in
  feature suite can exercise the same profile without turning scheduler pressure into a false
  release failure. The full core suite remains 530/530.
- Fixed the first Unix cross-target compile leak found by the new gate: the daemon's Windows-only
  named-pipe security descriptor is now imported only under `cfg(windows)`, while Linux and ARM64
  type checks remain explicit and warning-only for platform-specific dead code.

## [0.23.0] — 2026-08-08

### Changed

- Re-imagined the workspace pages with a cohesive dark/light visual system and carried the new
  design through the launcher, queue, projects, working sets, history, diagnostics, preflight,
  review and terminal surfaces. The headless Chrome audit is clean at both supported widths and
  both colour schemes.
- Split the frontend shell into focused launcher, queue, workspace-pages, terminal-history,
  operational-panels and daemon-events modules, reducing `web/src/main.js` below 2,500 lines
  without changing the source contracts or behavior covered by the 398-test web suite.
- Split the daemon control plane into protocol, dispatch, client and test modules so transport and
  request routing have explicit boundaries.

## [0.22.0] — 2026-08-08

### Added

- The launch goldens are checked in the release gate, and the check now asks whether a flag
  *applies* rather than only whether it exists. `terminalai-probe verify-goldens` had shipped a
  release ago and was invoked by nothing — no script, no npm script, no cargo alias, no workflow —
  so the one check that asks the installed CLI whether it accepts our argv only ran when somebody
  remembered. `scripts/verify-release-metadata.ps1` now runs it as its fifth claim, and an agent
  that is not installed or prints no help is a failure rather than a pass: a release certified
  against nothing is the failure the gate exists for.

  The check itself could not have caught the budget defect, because it compared flag names against
  the help text as one flat string while the constraint — "(only works with --print)" — sits on a
  wrapped continuation line beside the flag. `help.rs` now groups a help text into per-option blocks
  by indentation and reads the restriction out of the block, so a flag whose own documentation binds
  it to a mode this argv is not in is reported as accepted-but-ignored. It found `--fallback-model`
  on its first run.

- The launcher's advanced disclosure goes through the catalog. Ninety-three labels, hints, option
  captions, placeholders and button captions were literal text with no `data-i18n` attribute, and
  the i18n gate reported full coverage the whole time — it checks that every catalogued message is
  referenced and that every attribute the shell uses is handled, and neither question can see text
  that is in neither place. A new check reads the shell's own markup and asks the opposite: every
  text-bearing element and every placeholder inside a dialog must get its words from the catalog or
  from a named runtime writer. It was baited both ways before being trusted — the first version
  passed for anything inside a dialog, because it accepted an ancestor's id as evidence of a runtime
  writer, and the dialog's own id satisfies that for every element in it.

  Two messages that were already catalogued rendered wrong: `localizeDom` assigns `textContent`, so
  a message wrapping its own `<em>` flattened the hint into body text. Both are split, and a third
  check fails if a `data-i18n` element ever gains children again.

- Eight functions with no callers are gone, and one turned out to be a leak worth wiring instead.
  `SessionStatus::colour` was a second status-to-colour table nothing consulted while the frontend
  used its own; `in_state_for`, `in_status_for`, `PriceTable::with_model`, `auth_holds` and
  `from_store_with_domain` were unreferenced; and `resolve_agent` was the one Tauri command the
  frontend never invoked, so it is gone along with its manifest entry and all three ACL grants.

  `forget_transcript` was the exception. Transcript readers are keyed by session id and each holds a
  file offset and a pricing accumulator, and nothing ever dropped one — a daemon that supervised a
  hundred sessions over a week kept a hundred readers for rows that no longer existed. It is now
  called from `remove_entry`, with a test that counts them.

- The probe's help is generated from its dispatch table. It was a hand-written constant beside a
  `match`, and the two had drifted: twenty-nine subcommands dispatched, twenty-six advertised, with
  `auth`, `exec` and `limits` reachable and undocumented — `auth` in the README too. Adding an arm
  without a help line no longer compiles, and the README's probe section now names every command.

- A launch can bound its own fan-out. Admission governs how many sessions run, what they may spend
  and what they may hold — and had no view of the one multiplier a *single* session controls: since
  Claude Code 2.1.216 a session runs up to twenty concurrent subagents by default, and with agent
  teams it can hold several separate agent instances. The launcher can now set the concurrent cap
  and state whether teams are allowed at all, delivered as environment variables because the agent
  offers no flag for either. Blank leaves the agent's own default; zero is refused rather than read
  as "no cap"; "refuse teams" is a value this launch sets rather than something inherited from
  ambient configuration. Codex, which documents no equivalent, is refused per the
  `LaunchError::Unsupported` rule rather than launched as if it had one.

- A row that leads an agent team names its teammates. Since agent teams, one supervised session can
  be a lead plus several *separate* agent instances, and the fleet showed that as one row with one
  status — defensible only while the operator can see what the row actually holds. The team's own
  configuration is read on the transcript timer, from the session id this tool assigned at launch
  rather than the one a hook reports later, and the names appear as a wide-row cell that exists only
  when there is a team: no team, no assigned id, an unreadable file, a shape this build does not
  recognise and an empty member list all render nothing, because "no team" and "a team of nobody"
  would otherwise read identically. A team directory that goes away takes the cell with it. The
  *count* of processes a row holds stays a measurement from the job object; this is the names.

### Fixed

- The four state-less dialogs can now say they failed. The rollup, broadcast, approvals and
  explainer dialogs render from state the window already holds, so they have no loading state and
  correctly never had one — but they had no error state either, and a renderer that threw left an
  open dialog with an empty body. The operator's read of that is "still loading", which it is not
  and never will be. Each now opens first and renders through a guard, so a failure appears as a
  stated alert with a retry inside the dialog it belongs to, and the stack reaches the console.
  Loading states are deliberately still not added where the data is already in memory. The fleet
  search, which does have a backend behind it, gains the same retry instead of a bare line of text.

- The WebdriverIO end-to-end gate passes, and its screenshots show the shipping window. It had
  never rendered the fleet, for three separate reasons stacked on each other. The build was a dev
  shell (fixed in v0.21.0). Then the mocks were never seen by the application: WebdriverIO's Tauri
  plugin registers them in `window.__wdio_mocks__` and tries to intercept
  `window.__TAURI__.core.invoke`, and that redefinition fails silently on this WebView2 — probed
  inside a live run, the global was still the real binding with eight mocks registered beside it. So
  every `fleet_snapshot` reached a daemon the test build deliberately does not have. The wdio build
  now consults the mock map first and falls through to the real binding, so an unmocked command
  cannot quietly become a passing assertion.

  The third cause was in the application and is a real defect at any time: `loadSnapshotNow` wrapped
  the snapshot call and the focused-pane reattach in one `try`, so a pane that failed to attach was
  reported as "daemon unavailable" and replaced the fleet the window had just loaded. Only the
  snapshot call decides that now; a pane that cannot reattach says so in a toast. The run also keeps
  its screenshots instead of deleting them, because a green gate with nothing to look at leaves the
  question it exists to answer on trust.

- A worktree the agent removed stops being named by its row. `WorktreeCreate` was managed and
  answered with a placement, but its counterpart was neither managed nor listed as deliberately
  unmanaged — so when the agent cleaned a checkout up, the row went on naming a directory that no
  longer existed until the daemon restarted. `WorktreeRemove` is now managed, and the removal is
  confirmed against the filesystem rather than read from a payload field: the event says a removal
  happened, and whether *this* session's checkout is the one that went is a question only the
  directory can answer. A removal elsewhere changes nothing.

- Every documented `notification_type` is recognised by name. The `Notification` hook is already
  managed, so its payloads were arriving; the classifier searched them for substrings, which meant
  `agent_completed` landed on the idle bucket because it contains the word "complete", and
  `agent_needs_input` — added in Claude Code 2.1.198, and the one notification whose entire meaning
  is "this session is waiting for you" — matched nothing and was dropped. Same shape as the
  `PermissionRequest` defect fixed in v0.18.0.

  All eight documented values are now matched exactly, ahead of the heuristics, which stay for the
  spellings Codex and future versions send. `agent_needs_input` and `elicitation_dialog` move the row
  to *Needs you* and into the approvals inbox; `auth_success` clears an expired-credentials hold
  without touching an `Unknown` reading or an account name it did not report; an answered
  elicitation is deliberately not an attention state. A `notification_type` this build does not know
  is logged rather than silently filed.

- A session's memory is measured over the job its cap is enforced over. `JOB_OBJECT_LIMIT_JOB_MEMORY`
  applies to every process in a session's job; the figure the row showed, and the one admission spent
  against, came from `GetProcessMemoryInfo` on the supervised pid alone. Since agent teams that is not
  a rounding error — teammates are separate Claude Code instances and up to twenty subagents run at
  once — so a team could be killed by its own limit while the row read "not limited" and the
  projection said there was room for another session.

  The job is now enumerated and its private commit summed, live rather than peak: the cap is enforced
  live, and a peak reading would mark a session limited forever after one spike with no way back
  down. The row reports how many processes the figure covers, and a domain that owns no job says so
  rather than implying a tree of one. The background execution policy follows the same list, so an
  unfocused team no longer keeps every teammate at foreground priority.

- The per-session spend cap is enforced instead of being handed to an agent that ignores it. The
  launcher's budget field became `--max-budget-usd` in an interactive argv, and Claude Code
  documents that flag as working under `--print` only — so a control that promised a cap bound
  nothing, and the header said in words that it did. Two settings rode on the same flag: the fleet's
  `TERMINALAI_DEFAULT_BUDGET_USD` had no other delivery mechanism, and `budget_enforced_agents`
  named Claude on the strength of it.

  The flag is gone from the manifest, so no launch of either agent can emit it again, and the golden
  fixture that used to pin it now pins its absence from a spec that still carries a budget. The cap
  itself is kept and enforced where the money is actually counted: the transcript ledger, which
  reads both agents. A session that reaches its cap keeps running and keeps its scrollback, and
  stops being given work — its prompt queue pauses with a stated reason and broadcasts skip it,
  while an explicit send still goes through, so an operator who decides to carry on can. The row's
  cost turns red and names the figure it was measured against. `budget_enforced_agents` now names
  every agent, because the enforcement is this tool's own rather than a claim about someone's flags.

### Changed

- The launcher no longer offers a fallback model. `claude --help` restricts `--fallback-model` to
  `--print` in the same words it restricts the budget flag, so the control set an argument the agent
  accepted and ignored in every session this tool runs. Unlike the budget there is no ledger
  equivalent — this tool cannot retry a turn on another model — so the flag is gone from the Claude
  manifest and the field is refused by name rather than launched with something that does nothing.
  Found by the goldens gate on its first wired run.

- The README describes what ships. It claimed the daemon "can hibernate idle sessions while
  retaining their rows" — hibernation has no implementation anywhere in the tree and is parked
  pending live evidence that `--resume` restores enough context — and badged the platform as
  Windows, macOS and Linux while the bundle produces NSIS and MSI only. The idle-session paragraph
  now describes the mechanism that does ship, EcoQoS and low memory priority across an unfocused
  session's job, and the platform is stated once: the shell is Windows-native, and the core, daemon
  and probe type-check for `cfg(unix)` without that being a claim they run there.

## [0.21.0] — 2026-08-08

### Added

- A work run can repeat on a cadence. The queue already ran one stored prompt across many projects
  on demand, refusing dirty trees, asking the fleet for one slot at a time and recording every
  outcome; a schedule decides only *when* to start one, and a scheduled firing goes through that
  same path — so every refusal it enforces applies without being restated, and there is no second
  launch path to keep in step.

  Three rules carry the design. **A machine that was asleep does not wake up owing eight runs**:
  missed occurrences are skipped rather than queued, because firing them in a burst would put the
  same prompt into the same repositories several times over, each landing on the last one's
  uncommitted work — and the count of what was missed is reported instead. **A firing never
  interrupts the run before it**, because starting a run replaces the previous one, and a schedule
  that fired over forty working projects would destroy the report the operator was going to read.
  **Every firing is written down, including the ones that did nothing** — a schedule the operator was
  not present for is only worth having if it can say what it did while they were away.

  The cadence is an interval rather than a wall-clock time of day: `std` has no local-time facility,
  and "every 24 hours" is one dependency cheaper than "at 02:00 local", which drifts by an hour twice
  a year regardless. The projects are recorded when the schedule is set, so an unattended run cannot
  quietly change what it targets as repositories are cloned; the prompt is recorded by name, so
  editing it takes effect and deleting it fails loudly.

- Agents that say how far along they are now drive the taskbar progress bar. The report is ConEmu's
  `OSC 9;4` sequence, which Windows Terminal and the xterm.js progress addon also understand, and it
  is decoded **in the core** rather than in the focused pane's renderer — there is one renderer and
  the fleet is thirty rows deep, so decoding it there would have given progress only to whichever
  session the operator happened to be looking at. Every session's bytes already pass through this
  process on their way to the ring and the grid.

  The scanner is stateful because a pty hands over whatever bytes were ready, so the sequence
  routinely arrives torn across two reads — including its terminator. Its buffer is capped, because
  an OSC string with no terminator otherwise grows for as long as the session lives. A malformed or
  unknown report is not a report: nothing is invented, and a session that never emits the sequence
  has no progress at all rather than a bar sitting at zero. A dead process stops claiming progress —
  a bar left at 60% on an exited row goes on saying work is under way, and nothing arrives to
  correct it.

  The window has one bar and a fleet has as many answers as it has agents, so the rule says what
  happens when they disagree: one reporter is shown exactly as it reported, several go
  indeterminate rather than being averaged into a number no agent said, and an error anywhere
  outranks both.

- The daemon's control plane is tested arm by arm. A coverage run on 2026-08-04 named
  `terminalai-daemon/src/lib.rs` (58.31% of lines) and `logging.rs` (31.29%) as the two modules
  whose figures were not explained by being entry points; re-running it named the reason, which was
  not a number: of roughly forty request variants, six had ever been dispatched in a test. The gaps
  that mattered were the ones the registry cannot cover because the rule lives in the dispatcher —
  the payload and fan-out caps on `EnqueuePrompt` and `Broadcast`, the byte ceiling a search reports
  having actually used, the megabyte-and-hour conversions `SetAdmission` performs in both
  directions, the live-rows-only filter that decides what a saved layout contains, and the rule that
  a worktree path is returned only to the hook that asked for one. Every id-taking request is now
  checked to refuse a session that is not there and to name it: an arm answering `Ok` for a missing
  row tells the window the action worked. Each new test was verified by applying the mutation it
  exists to catch and watching it fail. `lib.rs` is at 70.49% and `logging.rs` at 80.75%; what
  remains uncovered there is the transport, the Windows console handler and the arms that shell out
  to a real agent binary.

- `SessionRegistry::poison_state_lock`, behind the core's `poison-recovery-tests` feature and absent
  from every shipped build. The daemon refuses stateful requests against a poisoned registry and
  answers the rest, which is the behaviour its `panic = "abort"` compile guard exists to keep real —
  and it was unreachable from the daemon's own tests because the mutex is private to the core.

- The diagnostics panel's inputs are pinned: a record carries the fields of its own span scope and
  no other, the message is not duplicated into the field list, every field type arrives as a
  readable string, a viewer that closed is unsubscribed, and one that merely fell behind is not.
  The last pair are opposite failures — a leaked channel per closed window, or a GUI that goes
  silent for good and looks like a daemon that stopped logging.

### Changed

- The work run — the prompt library, the run across many projects and the schedule that repeats it —
  moved out of `main.js` into `workRunPanel.js`, the fourteenth module split from it, with the moved
  code otherwise unchanged. `main.js` is at 3,685 lines. One thing the move taught: an extracted
  module has to fit the 120-column limit while `main.js` is only held to a ratchet, so a seam drags
  its long HTML templates into a limit they never had to meet — the fix is to split them by
  concatenation, never to raise the ratchet.

### Fixed

- The end-to-end harness built the app without `custom-protocol`, and `tauri`'s build script computes
  `dev = !custom_protocol` — so every run launched a dev shell pointed at `devUrl` with nothing
  serving it, the window came up with an empty body, and all of WebdriverIO's assertions failed for a
  reason none of them named. The app crate now declares the feature and the harness asks for it. With
  the ACL enforcing for real, four `wdio:` plugin commands the capability files had never granted
  turned out to be denied on every poll; they are granted now. The gate still does not pass — the
  fleet list stays behind the first-run check — and that is on the roadmap with the probe output.

- The store-health test no longer fails on a machine that has run it before. It ends by creating a
  `blocker` directory where its next run needs to write a `blocker` file, the scratch directory is
  named after the process id, and Windows reuses those — so the test failed with "access is denied"
  for reasons unrelated to the code under test, roughly whenever a pid came round again. The scratch
  directory is now cleared on the way in.

## [0.20.0] — 2026-08-07

### Added

- `terminalai-probe verify-goldens` asks the installed CLI whether it accepts the argument vector
  the launch goldens pin. The goldens assert what this tool *emits*; whether the agent on this
  machine accepts it is a separate fact, and the gap is not theoretical — two roadmap items were
  blocked in v0.19.0 because the flags they needed postdate the installed Claude Code, and every
  golden stayed green throughout. It reads the fixture directory rather than naming files, so a
  third golden is covered without the check being edited.

  It found one thing on its first run: codex-cli 0.146.0 lists no `--verbose`. That flag reaches the
  argv through `LaunchSpec::extra_args` — operator passthrough, forwarded verbatim by design — so
  the fixtures now declare `passthrough_args` and the check reports only what this tool is
  answerable for. The tool owns the argv it constructs, not the arguments a person passes through it.

- `.cargo/mutants.toml` scopes `cargo mutants` to the pure policy modules — admission, restart,
  context, waiting, search, help and spend — as a periodic check that they are *asserted on* rather
  than merely executed. A large green suite says code ran; the arithmetic most likely to run without
  being asserted on here is exactly what the supervisor turns on. The pty and process-spawning
  modules are excluded by name and with a reason: a mutant there leaves an orphaned agent and a
  pseudo-console behind rather than failing a test. A full-tree run is explicitly not the goal.

- The terminal pane is its own module (`web/src/terminalPane.js`) — xterm construction, palette,
  theme following, the WebGL renderer and its fallbacks, refit and hyperlink opening. `main.js`
  drops from 4,060 to 3,930 lines.

  The larger change is underneath: thirty-five test files read `main.js` as a string, so every seam
  extracted broke them, and broke them in the direction that hides things — an assertion about the
  terminal pane silently stops covering the terminal pane the moment the pane is a different file.
  They now read the whole renderer through `appSource()`, so the assertions are about the renderer
  rather than about one of its files, with `shellSource()` for the few that genuinely concern the
  shell not duplicating a module. Reading everything immediately made the attribute-escaping guard
  cover code it never had: it recognised only `escapeHtml`, while every extracted module takes the
  escaper as a dependency named `escape`, so none of them had ever been checked.

- `aarch64-pc-windows-msvc` is type-checked alongside the Linux target by
  `scripts/check-cross-targets.ps1`, and compiles clean. This project's differentiator is Windows
  while it ships x86_64 only, so every Snapdragon-class machine runs the entire fleet — daemon,
  probe and both agents — under emulation; the Windows-specific code is `windows-sys` calls rather
  than intrinsics, so the port should be a target-and-bundle question rather than a code one, and
  compiling it is what turns that from an expectation into something the suite knows. The README
  now states that releases ship x86_64 only, because type-checking is not support and an untested
  second architecture is worse than an honest single one.

- `terminalai-core::help` decides, from a help text and an argv, which flags the help does not list.
  Scanning stops at `--`, so the initial prompt (which both goldens deliberately set to
  `--dangerously-skip-permissions`, to prove it is not re-read as a flag) is positional by
  definition rather than by a guess. Flag matching is on a whole-token boundary, because a substring
  test reports `--model` as present in a help that only documents `--models`.

- The shipped sidecars carry their own dependency manifest. `cargo auditable` embeds a compressed
  list of crate names and versions in a `.dep-v0` section, so `cargo audit bin terminalai-daemon.exe`
  answers "which crate versions are in this exe" from the exe — which is what an advisory response
  actually needs, and the only copy that travels with a download. A lockfile in the repository
  answers a different question and requires trusting that the artifact was built from it.

- `scripts/supply-chain.ps1` produces a CycloneDX SBOM per shipped executable and, first, verifies
  every one of them carries its embedded manifest. The verification is the point: an SBOM generated
  beside a binary describes the source tree, not the binary, so it would look identical if the
  auditable step had been silently skipped. The check is on the reported data, not the exit code —
  `cargo audit bin` exits zero for a binary with no manifest, having simply found nothing to say, so
  an exit-code gate would certify exactly the binaries this exists to catch. `terminalai.exe` is
  built by the Tauri CLI and carries no section; that is reported rather than passed over.

- The README states the provenance level as SLSA Build L1 and says why it is not higher: these are
  locally built artifacts, and L2 requires a hosted build platform.

- `terminalai-daemon.exe` and `terminalai-probe.exe` are now reproducible: two clean builds from the
  same commit produce byte-identical files, so a released unsigned binary can be confirmed by
  rebuilding it rather than by checking a signature that does not exist.
  `scripts/verify-reproducible.ps1` is the check. It cleans the whole target directory between
  builds — a second build that reuses the first's artifacts confirms the cache is consistent, not
  that the build is deterministic — and keeps both copies outside `target/` so a failure can be
  diagnosed against artifacts a clean has not eaten.

  It found the tree was *not* reproducible: exactly 20 bytes differed in a 3.6 MB executable, being
  the debug directory GUID and the PE `TimeDateStamp` the MSVC linker writes from the clock. Nothing
  in the compiled code differed. `.cargo/config.toml` now passes `/Brepro`, which derives both from
  the content. The README states that the installers are not reproducible and cannot be as
  templated: WiX generates a fresh `ProductCode` per build and NSIS output is LZMA solid-compressed.

### Fixed

- The daemon's admission limits are read from the operator's configuration by a function that can
  be tested. `AdmissionConfig::from_environment` read seven environment variables directly, and the
  environment is process-global — a test that sets `TERMINALAI_MAX_LIVE_SESSIONS` changes it for
  every test running beside it — so none of that parsing had ever been asserted on. Mutation
  testing made it concrete: every mutant in the function survived, including replacing the entire
  body with `Ok(Default::default())`. The fleet's session cap, spend ceiling, spend window and both
  memory limits could have been ignored outright and the suite would have stayed green.

  `from_lookup` now takes the lookup as a parameter and `from_environment` is the one line that
  reaches for the real environment — the same shape as the clock injection already used across this
  crate. Nine tests cover it, including that `none`/`off` disables rather than parses, that
  megabytes are multiplied rather than added, that a zero session cap is refused instead of being
  silently clamped up to one, and that a negative budget is refused instead of being filtered to
  `None` and reading as "no cap configured".

- The browser chrome audit now measures the fleet — the main view of the application, which it had
  never checked. With no backend answering, the preflight call rejects, the app enters preflight
  mode, and `#fleet-list`, `#fleet-state-strip` and `#column-labels` are all `view-hidden`; every
  element inside them failed the visibility test and was skipped. The script reported clean on
  sixteen dialogs and menus while the screen the operator actually looks at went unmeasured. A
  minimal backend stub, installed before the app's module runs, answers two commands so the real
  render path reaches a populated fleet: one row per status, plus the unread, pinned, focused,
  limited-memory, full-context and too-long-to-fit variants.

- The audit checks both row densities. Wide is not the same row with more space — `.fleet-list-wide`
  is what un-hides the branch, the allocated ports and the status label, and it adds the model,
  effort, cost, memory and context cells. Auditing only the default left half of every row's text
  behind `display: none`, which to this script is indistinguishable from clean. A deliberately
  low-contrast branch cell is now caught; before this change it passed.

- Deliberate elision is no longer reported as an overflow. `text-overflow: ellipsis` requires
  `scrollWidth > clientWidth` to do anything at all, so the old rule called the 28px row's entire
  design a defect 140 times over. Content cut off by `overflow: hidden` with no ellipsis and no
  scrollbar is still reported, because that is the case where the operator is given no sign that
  anything was lost.

### Changed

- The design reasoning is published, in the module documentation of the code that implements each
  decision rather than in a separate design document. `cargo doc --no-deps -p terminalai-core -p
  terminalai-daemon` is the entry point and the README says where to start. A design document is a
  second place to be wrong — nothing fails when the code stops matching it — whereas a module's own
  docs sit next to the thing they describe and are read by whoever is about to change it. Two
  decisions that had never been written down anywhere tracked now are: why a process is contained
  one syscall after it exists, what escapes in the 34.8 µs gap and why the pty crate is not forked
  to close it; and why none of the daemon's three log sinks is unbounded.

- Each fleet state chip names its status in `data-status`. The audit reads those to discover every
  status the fleet models and builds its fixture from them, so a status added to the app is
  contrast-checked without the gate being edited — and a gate whose coverage is a hand-written list
  certifies whatever is missing from it. Every surface must also now measure at least one piece of
  text, since a surface that measures nothing reports exactly like a surface that is clean.

## [0.19.0] — 2026-08-07

### Changed

- Recorded that this project ships English only. The single-locale Fluent setup looked like
  scaffolding waiting for a second language, and a reader could reasonably have started building
  one. It is not waiting: the catalog buys one source of truth for the daemon and the renderer, and
  the Rust side is the only one that rejects a duplicate message identifier — the JS loader silently
  takes the last definition, and a duplicate has shipped once and was caught by that check alone. So
  the abstraction is not idle and removing it would cost both of those. A test fails if a second
  catalog appears, so adding a locale is a deliberate change to the decision rather than a
  half-built mechanism nothing selects between.

- The README says plainly what installing an unsigned build looks like: the SmartScreen prompt and
  the way through it, that reputation is built per file hash and therefore resets every release, that
  a self-signed certificate is rated identically to no certificate, that Smart App Control can block
  the daemon *after* a successful install, and how to verify a download by hash since there is no
  signature to check. No claim is made that reputation improves over time, because for an unsigned
  build it does not.
- The repository has its own `.github/` surface: a bug form that asks up front for the three facts
  every finding in this project has needed — the daemon log path, the agent versions and the Windows
  build — plus display scaling, since most measurement defects here have been scaling-related. It
  asks callers not to paste session output, which is their own source and sometimes their
  credentials. Security reports are routed to private GitHub Security Advisories rather than to a
  public issue.

### Fixed

- Choosing Claude in the launcher no longer rewrites a plan-mode selection to "ask". The reset had
  been there unchanged since the first Tauri shell commit, with no test and no recorded reason, and
  it rewrote two of this tool's own built-in presets — "Claude · Plan first" and "Claude · Quick
  question" — the moment the launcher synced its fields. Verified against the installed build rather
  than the documentation before removing it: `claude --help` lists `plan` among the accepted
  `--permission-mode` choices, and `claude --permission-mode plan --print` runs and exits 0.

### Added

- The app registers for restart after a crash, a hang or a Windows update. The daemon is designed to
  outlive its window and reattaching to it is already built and tested, but nothing told Windows
  that — so a forced restart dropped the operator into an empty desktop with a fleet still running
  behind it. The sessions survived; the only thing that did not was the way to see them. Registration
  happens at startup, which is the point: shutdown is exactly the moment a crashed process does not
  get. A failure is logged and never fatal.
- The app declares per-monitor-v2 DPI awareness explicitly, before any window exists, and then reads
  back what the process actually has. Awareness is a process property decided by whoever declares it
  first, and nothing here declared it — so the value was inherited rather than chosen. The failure it
  prevents is silent by construction: an under-aware process is not told the truth about monitors, so
  window rectangles and system metrics come back virtualized on a 125% display while still looking
  plausible and self-consistent. The effective value is logged, at `warn` when it is not what was
  asked for, because a call whose failure is an error code nobody looks at is the same mistake in a
  different place.
- `CwdChanged` is ingested, so a session that moves stops being described by where it used to be.
  The row's folder and branch both name where the session *is*, and neither was updated: a moved
  session went on naming a directory it had left and a branch belonging to that directory, quietly,
  for the rest of the run. The branch is dropped rather than left to expire — thirty seconds of
  naming the wrong branch is thirty seconds of the row being wrong about which work is where — and
  re-read on the next event, because the new directory is only known inside the state lock and this
  tool does not shell out to Git while holding it. Moving is not a lifecycle transition, so the
  status is untouched; the move appears in the status history instead.
- `WorktreeCreate` is answered with a path under this tool's worktree root, so a checkout the agent
  makes is one the supervisor owns rather than a stray for the survey to find later. It uses the
  same naming rule as a checkout this tool creates itself, which is what makes the existing cleanup,
  survey and land gate understand it. No worktree root configured, or an unknown session, means no
  answer: declining to place a checkout is safe, and naming a path that cannot then be managed is
  not. Every other hook stays fire-and-forget — an adapter that wrote to stdout on an ordinary event
  would be handing the agent a directive it never asked for.
- The hook events this tool deliberately does not manage are now listed with a reason next to the
  ones it does, because "not handled" and "handled by doing nothing" look identical from outside.

## [0.18.0] — 2026-08-07

### Added

- The spend tooltip states how old the embedded price table is, and marks the figures past 90 days.
  Prices are vendored from a pinned upstream commit and nothing aged them, so a table months out of
  date reported spend with exactly the same confidence as a current one. The only date the table
  carried was inside a prose version string; it now travels as a field. Nothing is fetched at
  runtime — the offline-by-design decision is unchanged, and the age comes from the embedded date.
  A table with no usable date says so rather than being called current: that is the hardcoded
  fallback, and calling it fresh would be the most confident possible statement about the least
  trustworthy table.
- Origin mode (DECOM). `CSI ?6h` was parsed and silently dropped, which is the worst of the three
  options: a program that sets it and then addresses the cursor relative to the top margin draws in
  the wrong place, and nothing anywhere said so. Cursor addressing is now relative to the top margin
  and confined to the scrolling region, setting or resetting it homes the cursor, and DECALN
  releases it along with the margins. Covered by conformance fixtures for both the set and reset
  states, each baited separately — the offset alone would still let a program address its way out of
  the region it had just declared.
- `await_session` over MCP: block until a named session reaches a given state, or until a timeout
  expires. The primitive an agent needs to coordinate with another instead of polling and guessing.
  Waiting is a read — no write token, and a wait never types into a session, wakes one or answers a
  prompt — so the read-only server serves it, which matters because the coordinating agent is
  usually not the one holding the operator's write token. An unsatisfied wait returns at once with
  what is left of the caller's budget and a retry hint, rather than sleeping: the server reads stdio
  one line at a time on one thread, so a tool that blocked would stall every other agent's read on
  the same process. The retry carries `remaining_ms`, so the total wait is a real bound instead of a
  clock that restarts on every attempt. A wait on a session that does not exist says so immediately
  rather than timing out, because a caller has to be able to tell a slow condition from an
  impossible one.
- An approvals inbox: every session waiting on a decision, longest wait first, with what it is
  asking. `NeedsApproval` said a session was blocked; nothing said on what, because the hook parser
  dropped the tool name entirely and the Codex app-server event's `kind`, `method` and `params` were
  all discarded on the way to the row. Both now reach it, through one summariser so the two agents
  cannot describe the same thing differently.
- The inbox never answers on the operator's behalf. There is no approve-all and no bypass toggle,
  and no universal "yes" is invented — what an agent accepts is its own prompt's vocabulary, so the
  answer is typed and sent to that session's prompt exactly as if it had been typed there. A session
  whose request cannot be described is still listed: that is the one to go and look at.

### Fixed

- A hook event named `PermissionRequest` that carried no `notification_type` parsed as a generic
  notification, which the registry ignores — so a session blocked on a permission prompt went on
  reading as `Working`. The event name is now taken as the kind it names; an explicit
  `notification_type` still wins.
- Frontend assertions that read Rust source no longer depend on line endings. Git checks this
  repository out with CRLF on Windows, so a pattern spanning two lines matched or failed depending
  on how the file reached the disk rather than on the contract being asserted.

## [0.17.0] — 2026-08-07

### Added

- Working sets: a named layout of many sessions, saved and relaunched as one action. Restoring
  starts each member through the ordinary launch path, so admission, the memory budget, the spend
  ceiling and the dirty-tree refusal all apply — and so will the next limit added, which a bespoke
  restore path would bypass silently on the day it appeared. A refused member is reported rather
  than forced, and the other eleven of twelve still start. No worktree path and no branch is saved:
  a private checkout is created fresh, because two sessions sharing one is the failure that feature
  already refuses and a saved layout is the easiest way to arrange it by accident.
- Find-in-pane in the focused terminal, with the match count `@xterm/addon-search` reports rather
  than a recount that could disagree with its own highlights. A scan still running says so instead
  of reporting zero matches, because those are different answers and one arrives before the other.
  The highlight colours are read from the theme tokens, not written as literals: decorations paint
  into the same canvas no DOM contrast gate can see.
- Fleet search. The daemon can be asked which sessions printed a string and how many times, over
  the retained scrollback each one still has on disk — the reason the disk tier exists, since the
  other twenty-nine sessions have no renderer to search. Available in the app, and as
  `terminalai-probe search <needle>`. Not exposed over MCP, which states that session transcripts
  are never exposed.
- Escape sequences are removed before anything is matched. Retained output is a rendered TUI, so
  searching the raw bytes both misses (a word the agent coloured has an SGR sequence inside it) and
  invents (`31m` is in every red thing ever printed). A sequence cut off by the spool's tail
  boundary emits no fragment, and a bare carriage return is treated as a line break so a redraw
  cannot join two lines into a match that was never on one.
- The wide fleet row shows how full each session's context window is, with an em dash when nothing
  has measured it. Occupancy is the **last request's prompt**, never the running token total: the
  totals column sums every request a session ever made, so a twenty-turn session divided by its
  window would report several hundred percent of a window it is nowhere near filling. A window is
  reported by the agent or not shown at all — a guessed denominator would put a percentage next to
  a number nobody can check.
- Codex's `thread/tokenUsage/updated` events now reach the row. They were parsed and discarded,
  including the only context window either agent states about itself. The event's `last` object is
  read for occupancy and its `total` object is not, for the reason above; an event carrying no
  `last` leaves the previous reading alone rather than replacing it with a zero.
- Compaction is visible in a session's status history. An agent compacting mid-turn is `Thinking`
  on both sides of a pause that can run to tens of seconds, so the transition-only history recorded
  nothing at all for it — indistinguishable from a stall. Both compaction hooks now record an event
  whether or not the status moves, and the row counts how many times it has happened.

### Fixed

- A finished compaction drops the occupancy reading it invalidated. The window is smaller than the
  last measurement said by an amount only the agent knows, so keeping the figure left the row
  claiming pressure that had just been relieved.

## [0.16.0] — 2026-08-07

### Added

- The fleet says so when its state stops reaching disk. A failed session-store write was an
  `eprintln!` on the daemon's stderr, which nobody is watching once the daemon is a background
  process — so every subsequent row change was silently unpersisted and the operator only found out
  by restarting into a fleet that had reverted. The failure now rides the snapshot to a banner
  beside the store-quarantine one, and clears itself on the next successful write. It is
  deliberately not dismissable: a quarantine is a past event to acknowledge once, this is an ongoing
  condition, and dismissing it would hide a live problem.

### Changed

- The update check answers in place. "A newer version exists" is the only outcome the operator can
  act on, and it arrived as a toast: gone in four seconds, no link, no way back to it — while the
  message itself said to download from GitHub. It now renders under the check button in the app
  menu, with a working releases link, and the menu stays open on the click that asks the question.
  The up-to-date and error outcomes need no action and remain toasts.

### Fixed

- A store claiming a row is both live and archived is normalised on load. The two lists are
  persisted and restored independently, so a hand-edited file — or one written by an older build —
  could say both, and archiving that row would have filed a second record of it. The live row wins;
  it is the one with a process history behind it. Previously the only defence was a guard at
  archive time that no test could reach.
- The focused terminal follows the theme. Its palette was a literal in the renderer, so the one
  surface that fills most of the window ignored `prefers-color-scheme` entirely: in light mode a
  light panel framed a hard dark rectangle, and focusing a session flipped the pane's apparent
  theme. The canvas is not DOM text, so no contrast gate could see it. The palette is now read from
  the same custom properties every other surface uses, the pane's own background is the terminal's
  so the two cannot drift apart, and a window whose OS theme changes repaints. The light ANSI set
  comes from the contrast-tuned light accents rather than the dark theme's pastels, and every colour
  an agent prints in clears 4.5:1 against its own background in both themes.
- The two overflow panels no longer claim ARIA menu semantics they do not implement. They were
  `role="menu"` with `role="menuitem"` children, but the app panel holds a `<select>` and a heading
  — invalid children of a menu, which makes a screen reader announce the wrong item count — and
  nothing implemented the arrow-key movement the menu pattern requires once the role is taken. What
  is actually implemented (a trigger with `aria-expanded`/`aria-controls`, Tab through the contents,
  Escape and outside-click to close) is exactly a disclosure, so the roles went rather than the
  behaviour.
- The HTTP hook reader's request deadline now bounds the body as well as the headers. `read_exact`
  armed the socket timeout once and looped internally, so every individual read got the full
  two-second timeout and each successful byte re-armed it — a local client declaring a megabyte and
  trickling one byte at a time held a worker indefinitely, before the bearer check, which runs on
  the fully-read request. Four such connections could stop hook ingestion for the whole fleet, and
  status updates would simply stop arriving.
- The memory budget stopped counting a session the moment a provider rate-limited it. Releasing the
  admission *slot* is right — a session the provider is refusing must not keep a queued one waiting
  — but the same filter fed the memory projection, so an agent process still holding ~509 MB
  vanished from it. With a budget configured the gate would admit roughly one whole agent of extra
  work per rate-limited row, and the machine went over exactly when the windows reset and every
  session resumed at once. Slots and residency are now counted separately, and the empty-fleet
  exemption asks whether anything is resident rather than whether anything holds a slot.

## [0.15.0] — 2026-08-07

### Added

- Model-based tests over the registry and session store. A reference model with no threads, pty,
  disk or clock runs alongside the real registry over generated operation sequences — archive, pin,
  focus, mark read, record a landing, queue a prompt, and persist-and-reload — and every field the
  operator can see is compared after each step. A divergence shrinks to the shortest sequence that
  still produces it. Scoped to the deterministic surface: `launch` and `kill` are asynchronous by
  design, so including them would mean waiting for quiescence between operations, which is how a
  state-machine test turns into a sleep-based one that fails under load.
- `focus_is_not_part_of_what_the_daemon_persists` states a contract that was previously only
  implicit: a reloaded daemon has no window attached, so nothing is focused, and the operator's next
  click sets it. The row itself survives; only the view's focus does not.

## [0.14.0] — 2026-08-07

### Added

- A VT conformance corpus for the terminal grid. Eighteen fixtures under
  `crates/terminalai-core/tests/fixtures/vt/` each carry a byte stream, the grid a conforming
  terminal must hold afterwards, and the rule that decides it — scrolling regions and IND/RI at the
  margins, ICH/DCH/ECH, IL/DL, tab set and clear, the alternate screen, autowrap, insert mode, ED,
  wide characters, combining marks, REP and DECALN. Expectations are derived from ECMA-48 and
  DEC STD 070, not captured from this implementation: an expectation taken from the code under test
  only pins today's behaviour and would ratify a bug as the standard. Every case is also replayed at
  six chunk sizes, because a pty hands over whatever the last read contained and an escape sequence
  arrives split as often as whole.

### Fixed

- `CSI n @` (ICH) was parsed and dropped. The screen already knew how to insert blanks — insert mode
  has always used it — but nothing dispatched the sequence, so an agent inserting characters got
  nothing. Found by the corpus above on its first run.
- `ESC # 8` (DECALN) was ignored. It is a screen alignment pattern, so a program that sends it
  expects the whole page overwritten and the margins released; ignoring it left stale text under a
  scrolling region that was never let go.

- A hook now reads the clock **once** for the whole event. Applying one hook made four separate
  `SystemTime::now()` calls under one lock — the quota reading's `reported_at`, its computed
  `resets_at`, the expiry check, and the notification observation — so a single event could stamp
  four different times. `apply_hook_at`, `apply_hook_with_token_at` and `apply_agent_event_at` take
  the instant, and the existing entry points read the clock and delegate.
- `a_reset_window_returns_the_row_to_the_fleet` never exercised the reset window. It asserted that
  the row left `RateLimited`, which happens on **any** provider signal regardless of the window,
  because an agent emitting tool events is by definition not being refused. The window's real effect
  is whether the stored reading is still held, and that is what the test now asserts — at one second
  before the reset and at the reset. It also stops sleeping 20 ms hoping the wall clock moved, which
  on Windows is a coin flip against a 15.6 ms tick.

## [0.13.0] — 2026-08-07

### Changed

- The supervisor's two policies are now modules of their own that decide over state passed in, with
  no lock, no thread and no clock. `admission` answers whether the fleet may start anything new from
  the configuration and a summary of what it already holds; `restart` answers what one process exit
  means from the exit code, the restarts already spent and how long the process ran. Both were
  previously private functions reaching into live registry state, so neither could be exercised
  without building a fleet. Every symbol is re-exported from where it used to live, so no caller
  changed.
- `registry.rs` was 6,106 lines and the highest-churn Rust file in the tree. It is now a directory:
  `mod.rs` owns fleet state, process lifecycle and the event stream, and six siblings own one
  concern each — agent-event ingestion, the scrollback tiers, the prompt queue and broadcast, lease
  and worktree provisioning, memory and cost sampling, and shared test fixtures. Submodules are
  children rather than crate-level siblings so the registry's internals stay private instead of
  being widened to make a split possible. Behaviour is unchanged and no assertion was edited.

### Fixed

- The slow-setup launch test asserted that `launch` returned inside one second. The setup hook it
  must not wait for sleeps about four, so the bound proved the point on an idle machine and nothing
  at all on a busy one — it is the test that failed under the last two release bumps, always while
  cargo was still compiling and always green on its own. The bound is now derived from the hook's
  own duration.
- `poll_transcripts`' documentation sat above `sample_memory`, so one was documented as reading
  transcripts and the other was undocumented. Each is now on the item it describes.

## [0.12.1] — 2026-08-06

### Changed

- The fleet row's markup moved out of `main.js` into `web/src/rowMarkup.js`. It was the longest and
  least reviewable code in the tree — a single-expression template where a typo is invisible in a
  diff, rendering every row of the fleet. `main.js` drops from 62 lines over 120 columns to 54, and
  the new module has none.

  The rendered markup is byte-for-byte unchanged, and that is checked rather than asserted: the
  renderer was moved verbatim first, a spread of fixture sessions was rendered and hashed, and the
  same fixtures were re-rendered after the template was divided into concatenated pieces. A literal
  is only ever split outside an interpolation, and an interpolation is always taken whole, so the
  pieces rejoin to the original bytes.

  The first attempt was caught by exactly that check. Indenting the moved function shifted the lines
  *inside* the template literal, which are markup rather than source, so every row gained two spaces
  of indentation — 112 bytes across the fixtures, invisible in review and invisible to every test.

- Three row assertions now read rendered markup instead of grepping `main.js` for a contiguous run
  of source text. That was always a proxy — it passes when the source happens to contain the right
  characters and fails when a line is wrapped, neither of which is a fact about what the operator
  sees. Extracting the renderer made it importable, so `web/tests/rowFixture.mjs` renders a real row
  and the tests read that.

## [0.12.0] — 2026-08-06

### Added

- A landing is now recorded on the session that produced it, and carried into that session's archive
  entry. This is the one fact that separates a finished session from an abandoned one: everything
  else about them looks identical — no process, a checkout still on disk, a branch nobody merged — so
  the leftover-checkout survey had no way to tell them apart.

- Archiving a session after its work lands is opt-in, from a checkbox on the review surface. A
  refused or partial landing archives nothing, and archiving still obeys every existing rule: a
  running session is refused, and a worktree holding unmerged commits is kept and reported rather
  than deleted, because `worktree::remove` uses `git branch -d` and never `-D`. Whether it archived
  is reported back rather than swallowed — a landing that quietly did not archive looks exactly like
  one that did.

- The landing is refused from filing itself against a session that did not produce it. A request
  names both a source directory and a session, and nothing forced them to be the same thing: naming
  an unrelated session wrote a false landing record onto its row and, with the opt-in, retired that
  row on the strength of work it never did. Both were silent. The daemon now checks the named
  session's own tree — its worktree if it has one, its working directory otherwise — against the
  source, and refuses with both paths named. Found by driving the real binary, not by reading the
  code.

- `terminalai-probe land` gained `--session <id>` and `--archive-on-success`, so the whole path is
  exercisable headlessly. `--archive-on-success` without `--session` is a usage error rather than a
  silent no-op.

## [0.11.0] — 2026-08-06

### Changed

- The README says what a sandbox does and does not mean on native Windows. The em dash in the
  Claude Sandbox row read as "not mapped" when the truth is that neither agent has a first-party
  filesystem sandbox on this platform at all — Claude Code's is macOS, Linux and WSL2 only, and
  Codex's `--sandbox` constrains Codex's own tool calls rather than the OS. So the worktree and the
  environment lease are not an addition to a sandbox here, they are the whole of the isolation, and
  the built-in bypass preset now says that in its own description rather than leaving an operator to
  infer it.

- Agent identity and flag mapping are data, not code. Everything that differs between the two
  supervised families — label, command name, executable stem, version banner, npm package layout,
  the environment variable naming the agent's own config directory, and which launcher choice
  becomes which token in which position — now lives in `crates/terminalai-core/agents/builtin.toml`
  and is emitted by one manifest-driven builder. Two hand-written per-family argv functions are
  gone. The argument vectors are unchanged byte for byte, proven by the existing golden fixtures.
  What stayed in code is what is not spelling: the refusals, the resume-id shape check and the
  capability-probe protocols, which a manifest names rather than describes.

### Added

- `npm --prefix web run test:chrome` drives the real chrome in a real browser: it opens all nine
  dialogs, both overflow menus and the launcher disclosure at 1440px and 1100px in both colour
  schemes, and fails if anything overflows its container, if the page scrolls horizontally, or if
  any composited text falls below WCAG AA. Every other frontend test is jsdom, which has no layout
  engine and therefore cannot see any of that. It serves the frontend with Vite and drives headless
  Chromium, so it needs no packaged app, no daemon and no virtual display, and it is separate from
  the WebDriver suite that remains blocked.

  It found three defects on its first run, all invisible to the existing suites:

  - The app overflow menu anchored left from a button at the right edge of the header, so opening it
    pushed 156px past the viewport and gave the whole page a horizontal scrollbar. The tools menu
    already carried `menu-right`; this one never did.
  - `.button` and `.menu-item` declared a text colour, a transparent border and a transition on
    `background`, but no background in the rest state — so both fell through to the user agent's
    `ButtonFace`, a mid grey in a dark colour scheme that no token in the stylesheet ever chose.
    Text picked for the Catppuccin surfaces landed on it at 3.0:1 and 3.7:1. The `:hover` rules
    painting `var(--surface0)` show what was intended; the rest state was simply never written down.
  - The settings dialog's Apply button was the only dialog confirm in the markup without
    `button-primary`, which is why it had no background at all.

- The supervisor's health verdict is no longer a restatement of whether a PID exists. `SessionHealth`
  was recomputed from `status` and `pid` on every transition, so a session busy thinking and one that
  had wedged were the same thing to it — the documented cause of restart storms in every system that
  conflates them. Progress signals (a pty byte, a transcript append, an agent hook event) now extend a
  per-session deadline; three consecutive misses mark the session `unresponsive`.

  It is a report, never a restart. A silent agent may be thinking about a large repository, so only a
  proven-dead process is brought back — and the supervisor's own bookkeeping does not count as
  evidence of life, or the fleet would reassure itself by writing a status and reading it back.
  Distinct from the existing stall flag, which asks how long a status has been *held*: a session
  printing a build log for twenty minutes is stalled and perfectly healthy, which is the case the old
  model could not express. The fleet row says which of the two it is looking at.

  A store written by this version names a health state older builds do not know, so an older daemon
  will quarantine it — the same version-skew rule that already applies to the session store.

- The MCP server speaks the current protocol revision, `2026-07-28`, alongside the `2025-06-18`
  handshake it already spoke. The current revision deleted `initialize` and made every request
  carry its own version, so the server now answers the mandatory `server/discover`, negotiates from
  each request's `_meta`, returns `resultType` and its own identity on every modern result, and
  refuses an unknown revision with `UnsupportedProtocolVersionError` (`-32022`) listing what it does
  speak — a client has no other way to find a version they share. Cache hints on the tool listing
  and on discovery are `private`, because that listing varies with which sessions the operator
  opted in.

  The handshake revision was kept rather than dropped for one release: the clients this server
  exists for are Claude Code and Codex, and both still open with `initialize`. Which era answers is
  decided by what the request says, and a request that says nothing gets exactly the bytes it got
  before — asserted by a test, not assumed. `ping` is the one visible difference, gone in the
  current revision and still answered in the handshake one.

- The launcher maps the flags that control tools, MCP servers and plugins: `--allowed-tools`,
  `--disallowed-tools`, `--settings`, `--setting-sources`, `--mcp-config`, `--strict-mcp-config`,
  `--plugin-dir`, `--plugin-url` and `--fallback-model`, all verified against `claude --help` on
  2.1.170. An allowlist is the precise lever for permission-prompt fatigue — it answers the prompts
  in advance instead of turning them off wholesale the way bypass mode does. Codex expresses none of
  them on 0.146.0, so choosing one with Codex selected refuses the launch rather than dropping it,
  and the controls are hidden for that agent. Values that begin with `-` are refused before argv is
  built, and a plugin URL must be `http(s)` — it fetches and runs remote code. `--max-turns` is not
  mapped because Claude Code 2.1.170 does not have it, and Claude's own `--worktree` is deliberately
  not mapped because this supervisor places, names, gates and surveys worktrees itself.
- An operator can override the built-in manifests from `agents.toml` in their TerminalAI data
  directory. Manifests are trusted local configuration; a repository may not supply one, because a
  repo-declared agent definition is arbitrary argv arriving with a clone. A manifest listing a slot
  its flag table does not spell — or spelling a flag its order never emits — is refused at load with
  the field named, and an unusable file stops the daemon rather than silently reverting to the
  built-ins.

## [0.10.0] — 2026-08-06

### Added

- `scripts/verify-release-metadata.ps1`, a release gate for the claims a release makes about
  itself. v0.9.0 shipped with no changelog entry: the release commit renamed `## [Unreleased]` to
  `## [0.9.0]` and the next commit renamed it straight back rather than opening a new section, so
  the string "0.9.0" appeared nowhere in this file while four manifests and the README badge all
  declared it. The gate refuses a release whose declared version strings disagree, whose version has
  no changelog section, whose version sections repeat a subsection — the signature of appending to a
  released section — or whose README states test counts the suites do not report. Run by
  `scripts/verify-installer.ps1`, and standalone with `-SkipTests` for a fast metadata check.

### Changed

- The permission mode is open, the way reasoning effort already was. Claude Code has added `auto`,
  `dontAsk` and `manual` since this launcher's four modes were written, and a closed list did not
  merely fail to offer them: a preset or a resumed spec naming one was silently rewritten, and the
  launcher's `<select>` reduced it to an empty value that launched with no mode at all. An
  unmodelled mode now reaches the agent verbatim with a warning, is carried into the dropdown so it
  round-trips visibly, and keeps the stored spelling of the four modelled modes unchanged.
  Repository-declared templates keep the closed vocabulary on purpose — an operator choosing an
  unmodelled mode is informed consent, a file that arrives with a clone choosing one is not.

- Sessions can name their own agent config directory and the parent variables they inherit. The
  child-environment allowlist carries no credential of any kind, which is right as a default and
  left API-key, Bedrock and Vertex operators with an agent that could not authenticate and a symptom
  that read as an expired login. A launch may now set `CLAUDE_CONFIG_DIR` / `CODEX_HOME` — two
  directories are two accounts — and name parent variables one at a time. Nothing is inherited by
  being present in the parent: an unnamed variable never crosses, and a name that is malformed,
  reserved to the supervisor, or unset in this process refuses the launch instead of producing a
  session quietly missing its credential. `terminalai-probe start` takes `--agent-home` and a
  repeatable `--env-passthrough`.

### Fixed

- The v0.9.0 changelog section is restored, and the entries written after that release now sit under
  `[Unreleased]` where they belong rather than inside the released section.

- The README's stated test counts had drifted: 520 default Rust tests, 523 with all features and 287
  frontend tests, against suites that report 523, 526 and 300. The new gate is what will notice next
  time.

- Choosing "accept edits" for a Codex session put it in Codex's *most* interrupting approval mode.
  The control mapped to `--ask-for-approval untrusted`, which runs only known-safe read operations
  without asking and requires approval for anything that mutates state — so it prompted more than
  "Ask" (`on-request`) did, and the whole permission ladder ran backwards for one agent. The shipped
  "Codex · Build" preset, described as writing inside the workspace, was launching that way. Accept
  edits now maps to Codex's own documented auto preset: `on-request` approvals paired with the
  `workspace-write` sandbox. Asking for accept edits together with the read-only sandbox is now
  refused by name instead of launching a session that would fail on its first write. A test ranks
  each agent's emitted approval value by how often the vendor documents it as prompting and asserts
  the ladder cannot invert again; it fails against the previous mapping.

- The fleet toolbar overflowed its own panel and painted on top of the terminal pane. At the
  documented 1440×900 default window size, "Limits", "History" and the help button were drawn over
  the "No focused session" heading as overlapping, illegible text, and "Review" and "New session"
  were pushed off the right edge of the header. Cause: `.panel-toolbar` is a flex row whose children
  had no `min-width: 0`, so a flex item refuses to shrink below its content width and the surplus is
  painted rather than clipped. Both toolbars now allow their children to shrink.


- The launcher shows the four things that decide a launch and folds the other seventeen away. Agent,
  session label, project folder and the initial prompt are what a session is actually chosen by; the
  rest — model, effort, permission, sandbox, profile, resume, budget, web search, extra directories,
  template, worktree, both port fields and both hooks — now sit behind one **Advanced options**
  disclosure that starts closed. The summary lists what is inside it, so nothing has to be opened to
  find out whether the wanted field is in there. Every field kept its id and is still read by the
  same `readSpec`/`writeSpec` code, which a test enforces field by field.

- The chrome went from 21 controls to 9. The row that is always on screen now holds only what is
  used while scanning the fleet — the filter, the agent and status filters, "Needs input" and
  "Wide" — and the header holds "Review", "New session" and a menu. Everything else moved behind two
  overflow triggers: **Tools** (grouping, Projects, Prompts, Broadcast, History, Limits, and the
  explainer) and a header menu (presets, Refresh, Preflight, Check updates). Nothing was removed and
  every control kept its element id, so each one is still driven by the handler it always had. The
  four-button preset cluster became a labelled section with real words instead of `▶ × ↺`. The menus
  close on outside click and on Escape, return focus to their trigger, and close behind an item that
  opens a dialog — covered by ten tests against the real `index.html`.

## [0.9.0] — 2026-08-04

### Fixed

- The daemon's peer-PID check did not compile on Unix. `interprocess` reports the peer's process id
  as the platform's own type — a signed `pid_t` on Unix, `u32` on Windows — and the handshake
  compared it against the client-declared `u32`, so the connection-authentication path had a type
  error nothing on this machine could see. Found by compiling the non-Windows branches for the first
  time. The conversion now lives at the one boundary, and a negative `pid_t` (which names a process
  group, never a peer) becomes `None` rather than wrapping into a large positive id that could
  collide with a real one.

### Added

- Leftover session checkouts are surveyed and the safe ones can be reaped. Teardown deliberately
  keeps a branch that holds unmerged work — which is right — but nothing ever revisited it, so
  worktrees, branches and their registrations accumulated silently, and a registration outliving its
  directory makes every later `git worktree add` for that path fail. The history dialog now lists
  every checkout under this tool's worktree root that no live session owns, marking each fully
  merged, unmerged with a commit count, or unknown, and naming the ones that are registrations only.
  **Only a fully merged checkout gets a Remove button.** A branch holding commits is listed and left
  alone: this view cannot show what those commits contain, so offering to delete them here would ask
  for a decision on evidence it has not presented. `Unknown` is treated the same way — "we could not
  tell" must never resolve to "delete it" — and the refusal lives in the core, so a caller that
  skipped the window cannot delete commits either. Only branches under this tool's own `terminalai/`
  prefix are surveyed, so a worktree an operator put under the same root by hand is never offered.
  `terminalai-probe worktrees` drives the survey headlessly.

- A history of finished sessions. The store has written an archive record for every retired row
  since the first release — id, agent, label, folder and the exact command — and read it back only
  to advance the id counter, so a finished-work view existed as data with no renderer. **History**
  in the toolbar lists them newest first with the command that produced them and offers to launch
  one again. The relaunch fills in the agent, label and folder and says so: the archive keeps the
  command as text, so restoring a model or a sandbox from it would mean parsing an argv the record
  never promised to keep parseable. A record written before archives carried a timestamp shows an em
  dash rather than a date in 1970. `terminalai-probe archives` and `terminalai-probe archive <id>`
  drive the same requests without a WebView. The archive is read-only by construction — the response
  carries no handle to anything live.

### Changed

- A managed `stable-x86_64-pc-windows-msvc` toolchain is installed alongside the standalone one, so
  targets and components can be added at all — the linked `terminalai` toolchain accepts neither.
  It is deliberately **not** the rustup default: making it so would shadow the standalone install for
  every shim-resolved tool. `cargo llvm-cov --workspace` now produces a report (72.15% of regions,
  71.28% of lines), run once to find untested arms rather than to chase a number; what it found is
  filed on the roadmap. The suite only runs under coverage because the lease-environment allowlist
  test now ignores the profiling runtime's own `__LLVM_PROFILE*` variables, which the instrumented
  child sets on itself *after* `env_clear()` — they were never inherited, and reading them as a leak
  would have made the strict allowlist look broken when it is not.

- The non-Windows code paths are compiled. 24 `cfg(unix)` / `cfg(not(windows))` branches across
  eight files had never been type-checked, because no non-Windows target was installed — so an error
  in any of them survived a full green suite, which is how the peer-PID defect above lived here.
  `scripts/check-cross-targets.ps1` builds `terminalai-core`, `terminalai-daemon` and
  `terminalai-probe` for `x86_64-unknown-linux-gnu` and exits non-zero on failure. Two pins in it are
  load-bearing and were each found the hard way: the toolchain must be a managed rustup one, because
  the linked standalone install refuses to add targets at all; and `RUSTC` must be set explicitly,
  because that standalone install sits earlier on PATH and cargo would otherwise drive a rustc whose
  sysroot has no Linux std — reporting the target as missing when it is installed. `terminalai-app`
  is excluded and says so: on Linux its Tauri tree pulls `libdbus-sys`, whose build script needs a
  Linux pkg-config that cross-checking from Windows cannot supply.

- The stylesheet is one declaration per line. `styles.css` shipped as 293 lines whose longest was
  2,204 characters, with whole rules concatenated onto one line — and a literal `\n` inside one of
  them had already taken out the Review view's only layout, in a form no review could have caught.
  It is now 2,835 lines with a longest of 92, verified byte-identical after collapsing whitespace and
  comments so the reformat provably changed no rule. A new `lineLength.test.mjs` states the limits
  (100 columns for stylesheets, 120 for frontend modules), rejects a stylesheet line carrying more
  than one declaration, and holds `main.js` to the long-line count it already has so the remaining
  debt cannot grow while its renderers are extracted. Five CSS assertions that had hard-coded the
  one-line rule shape now match the rule regardless of formatting — they were asserting layout where
  they meant to assert intent.

- The window in which a newly spawned agent is not yet inside its job object is down to a single
  syscall, and is now measured rather than assumed. Containment previously created the job, applied
  its limits and only then assigned the process — three syscalls after `CreateProcessW` returned,
  during which anything the agent spawned escaped the kill-on-close guarantee every teardown path
  relies on. The job is now created and configured before the process exists, leaving only
  `AssignProcessToJobObject`: **34.8 µs**, measured on Windows 11 26100 and pinned by a test that
  fails if a syscall creeps back in front of it. Membership is read back with `IsProcessInJob`
  instead of being inferred from the assignment's return value, because a containment guarantee that
  quietly did not apply is worse than one that failed loudly. The window cannot be closed entirely
  without owning the `CreateProcessW` call — `PROC_THREAD_ATTRIBUTE_JOB_LIST` needs a second
  attribute slot and `portable-pty` sizes its list for one — and forking the pty crate was rejected
  as a permanent maintenance cost on the fleet's most load-bearing path.

- The archive of finished sessions is bounded. Every archived row was appended to a list with no
  cap, and that list is serialized into each full store snapshot — which is written after a 200 ms
  quiet period and at least once per second under sustained output. Persistence cost therefore rose
  monotonically for the life of an install, on the hot path. Archives are now bounded by count
  (`MAX_ARCHIVES`, 200) and by age (`ARCHIVE_MAX_AGE`, 30 days), trimmed oldest-first where the list
  grows and again when an oversized store is loaded. A record written before the bound existed
  carries no timestamp, and an absent timestamp is *not* read as the epoch — it ages out through the
  count bound instead, so upgrading does not delete the history it was meant to bound. The next
  session id is still computed over every archive before trimming, so an id that has been issued is
  never issued twice.

## [0.8.0] — 2026-08-04

### Added

- A fleet-wide spend ceiling. A per-session budget bounds one agent; nothing bounded the fleet, so
  twenty sessions each obeying a $5 cap could spend $100 while every individual limit reported
  itself satisfied. `TERMINALAI_SPEND_CEILING_USD` sets the ceiling over a rolling window
  (`TERMINALAI_SPEND_WINDOW_HOURS`, 24 by default). Reaching it stops anything new from starting and
  never touches a running session — the ceiling is an admission gate, not a kill switch. Spend is
  recorded as deltas, because a session reports a running total and the window has to count the
  money once and count it when it was spent; the ledger buckets by minute, so a window costs at most
  one entry per minute no matter how chatty the fleet is. It is persisted with the session store, so
  restarting the daemon is not a way to clear the ceiling. The header states the ceiling, how much
  of it is used, and — because only Claude takes `--max-budget-usd` — which agents a per-session
  budget actually binds, rather than implying a hard stop that half the fleet does not have.
- Memory-aware admission, and job objects that limit rather than only contain. The job has always
  reaped a session's process tree, but `KILL_ON_JOB_CLOSE` alone let one leaking agent take the
  machine down with every other session still nominally healthy. Sessions are now created inside a
  job carrying an optional per-session memory cap (`TERMINALAI_SESSION_MEMORY_CAP_MB`) and process
  count (`TERMINALAI_MAX_PROCESSES_PER_SESSION`), and `TERMINALAI_MEMORY_BUDGET_MB` gates admission
  on projected fleet commit. Each live session's private commit is sampled on the transcript poll —
  `PrivateUsage`, not working set, because Windows trims a working set under pressure so a leaking
  agent can show a *falling* one while its commit climbs — and shown on the wide row. A session that
  has not been sampled is projected at its agent's measured typical size rather than at zero, since
  admitting on "we have not looked yet" is how a machine gets oversubscribed. Admission requires
  headroom for one more session rather than blocking once the total is already over, because the
  latter admits exactly the session that puts it there; an empty fleet always gets one session, so a
  budget too small for any agent surfaces as a stuck row instead of a silently halted fleet. A
  session at its cap is reported as memory-limited rather than left to look like an ordinary crash.

- Fleet-wide detection of expired agent credentials. Running many sessions at once provokes OAuth
  refresh races, and an agent whose login has gone looks exactly like a busy one: it takes a prompt,
  works briefly, and fails. The daemon now asks each installed agent every five minutes
  (`claude auth status --json`, `codex login status`) and raises one banner naming the agent instead
  of letting a work run turn one expired login into one failure per project; queued work holds until
  it is resolved and running sessions are untouched. Only an explicit "not logged in" holds anything
  — a probe that could not run reports `unknown`, which blocks nothing, because a banner the
  operator cannot clear by signing in is worse than no banner. `terminalai-probe auth` runs the same
  path the daemon does.

- A settings surface for the daemon-wide limits. The product is a dense fleet list and the shipped
  default admits three sessions, which until now could only be changed by setting an environment
  variable before the daemon started — a default that contradicted the pitch and a control nobody
  would find. **Limits** in the toolbar edits the admission cap, the default session budget, the
  spend ceiling and its window, the fleet memory budget, the per-session memory cap and the process
  count, and applies them to the running daemon: a raised cap admits from the queue immediately, a
  lowered one simply stops granting, and nothing already running is touched. An empty field means no
  limit rather than a limit of zero. Environment variables stay the boot default and the dialog
  names which ones it started from, because a value the operator did not type came from somewhere.
  `terminalai-probe limits` drives the same requests headlessly.

### Changed

- The workspace's direct `windows-sys` dependency moved from 0.59 to 0.61, which is the prerequisite
  for real job-object limits and drops the large import libraries in favour of unconditional
  raw-dylib linking. `Win32::Foundation::BOOL` no longer exists in 0.61, so the one window-enumeration
  callback that named it now uses the raw `i32` the contract has always been. The lockfile still
  carries a 0.59 copy: it is reached through Tauri's `window-vibrancy` and through `winreg` in a
  build dependency, neither of which this workspace controls, so the duplicate stays until upstream
  moves.

- Every admission site now asks one function whether anything may start. The slot cap and the
  ceiling were otherwise two checks in four places, which is how one path ends up enforcing a limit
  another ignores; the snapshot reports which of the two is blocking so a queued row says why.

## [0.7.0] — 2026-08-04

### Added

- Stalled sessions are detected, marked, and sorted to the top. The dwell timer was formatted and
  never compared against anything, and the ordering within `Working` was newest-first — so the
  session stuck longest sorted *last*, in the status the code's own comment calls "the long tail
  where sessions get stuck". A session holding a working status past fifteen minutes is now marked
  stalled by the supervisor, sorts above healthy working rows with the longest-stuck first, and
  raises one attention notification that retracts as soon as it moves. The row states the threshold
  rather than showing an unexplained badge. The flag is computed where there is a clock and stored
  on the session, so the fleet comparator stays a pure function of stamped values — a comparator
  that reads the clock can change its answer mid-sort and break the total order `sort_by` requires.

- A work run gives up on entries that have waited too long for a fleet slot. There was no deadline
  at all, so an entry queued hours ago launched whenever a slot happened to free — against a tree
  that had usually moved, a prompt that had been superseded, or work the operator had since done by
  hand. Entries past a two-hour deadline are now reported as expired, in their own outcome category
  beside done, failed and skipped, saying how long they waited: nothing went wrong, so calling it a
  failure would send the operator looking for a fault. A paused run does not age out. Dispatch stays
  in the order the operator wrote — newest-first was considered and rejected, because silently
  reordering a run makes a partial one cover a different set of projects than the top of the list
  suggests, and the deadline handles staleness visibly instead.

- A row waiting on a question shows how long is left before the agent answers it for itself. Claude
  Code's `AskUserQuestion` proceeds without an answer after sixty seconds, and the notification
  grace period spent thirty of them — so the operator was told at the halfway point and the agent
  decided on its own if they missed it. Question-like states now wait five seconds rather than
  thirty, which still filters the intermediate prompts a tool emits as it starts while leaving
  fifty-five seconds to reply, and the deadline is named in one constant the grace periods are
  measured against. Permission requests keep the longer grace deliberately: nobody but the operator
  answers one, so there is no deadline to race and no reason to trade away the de-noising.

- "Elsewhere on this machine" rows show what the agent said about itself. The panel already ran the
  command that returns Claude Code's own status vocabulary and then collapsed the answer to process
  liveness, so a row read "Running" while the agent had reported it was blocked on a permission
  prompt. `state` and `waitingFor` now reach the row verbatim, beside — not instead of — the
  liveness we determined ourselves. A blank or absent field stays absent rather than becoming an
  idle-looking row, and the rows remain actionless: this changes fidelity, not ownership.

- The fleet header shows quota headroom, so the fleet can warn before a window closes rather than
  only reporting that it has. Codex publishes a quota table continuously; the parser read
  `used_percent` out of it, correctly decided the window was not blocking, and then threw the whole
  reading away — the fleet had the number that would have warned first and dropped it twice over,
  once in the registry and once at the UI boundary. The most-consumed window across the fleet is
  now kept as headroom separately from an active refusal, and the header states its percentage,
  which quota it is and when it reopens. A fleet where nobody reported one says so rather than
  rendering 0%, and explains that only Codex publishes a table continuously.

### Fixed

- A session the supervisor gave up on no longer reads exactly like one that finished. Both arrive as
  status "exited", and the row rendered only the status — so a crash loop that had exhausted its
  restart budget and a job that completed both said "Exited — The process has ended". The row now
  consults the phase: a failed session is red and states how many restarts were spent and the last
  exit code, a finished one is green and says the agent ended its own session, and both carry the
  reason in the compact row's tooltip as well as the diagnostics drawer.

- The grid no longer gives zero-width characters a cell of their own. Combining marks, zero-width
  joiners and variation selectors were forced to width 1, so `e` + U+0301 occupied two columns here
  and one in every real terminal — and this grid is what the pinned split view draws. They now
  attach to the glyph they modify, stepping back over a wide character's filler half to land on the
  character rather than beside it. A mark with nothing to attach to is dropped, and a control
  character that reaches the same path is ignored the way a real terminal ignores it.

- Shrinking the grid keeps the newest rows instead of the oldest, and a resize no longer discards
  the scrolling region. Copying from the top-left threw away the cursor line and the last output —
  the part anyone is actually looking at — while xterm.js, drawing the same stream in the focused
  pane, reflowed and kept them; ConPTY's quirky resize means nothing re-emits the buffer, so the
  consumer decides what survives. A DECSTBM region set by a TUI agent is now clamped to the new
  screen rather than reset, because agents set it once and never send it again.

- Every production thread now starts through `thread::Builder` and degrades instead of panicking.
  The workspace standardised on the builder because `std::thread::spawn` panics exactly where the
  builder returns an error — thread exhaustion, which is what a thirty-session fleet with a reader,
  a writer, a monitor and a timer per session produces — and eight sites had been missed. A review
  that cannot start a worker runs with fewer and says so; one that cannot start any reports no
  reviews rather than blocking on a queue nothing drains; a land or review command that cannot
  capture its output refuses by name rather than deadlocking against a pipe nobody reads; the
  external-session panel degrades to empty; and a toast listener that cannot start costs
  click-to-focus rather than the window process.

- The restart budget is a rolling window rather than a lifetime counter. Five restarts spread over
  a week used to permanently kill a session that had run healthily in between, because nothing ever
  reset the count. A process that runs for ten continuous minutes now clears the budget — the
  number and the reason are Kubernetes' CrashLoopBackOff reset — while a genuine crash loop still
  exhausts it in five.

- Restart backoff is now fully jittered. Failures here are correlated by construction: one provider
  rate limit or one dropped network takes every session at the same instant, and a deterministic
  delay guaranteed all of them retried at the same instants against the service that had just
  refused them. The delay is drawn uniformly from zero to the same exponential ceiling, which is
  the variant AWS measured as spreading a synchronised fleet fastest. A random source that fails
  yields the full ceiling rather than zero.

- Stopping a session now gives the agent a chance to shut itself down. `TerminateJobObject` fired
  immediately, so the `SessionEnd` hook — one of the sixteen this app installs — never ran, and the
  transcript the fleet reads for cost never flushed its final usage records. The stop is now a
  ladder: the interrupt byte into the pty, then closing the pseudo-console, then the hard kill,
  bounded at five seconds in total and logged when the last rung is reached. Verified against a
  real Claude Code session, which now exits through its own interrupt path rather than being
  killed. Measured while building it, and recorded in the code: writing `0x03` to this ConPTY does
  **not** raise a console control event on Windows 11 26100 — `ping` ignores it entirely — so a
  single-rung ladder would have been a five-second pause in front of the same hard kill.
  `GenerateConsoleCtrlEvent` is unusable here because it addresses the caller's own console.
  The stop runs on a worker rather than the client's dispatch thread, so a five-second grace no
  longer freezes every other request on that connection, and the row shows the agent shutting down
  while it happens. Daemon shutdown stops the whole fleet concurrently for the same reason.

- A session that exits cleanly is no longer restarted like one that crashed. The exit code was
  captured and never consulted, so an agent that finished its work — or an operator who quit one
  inside its own pane — was brought back up to five times, billing quota on each. Classification is
  now one named function following the line every mature supervisor draws (OTP's `transient`,
  systemd's `Restart=on-abnormal`): exit 0 and `STATUS_CONTROL_C_EXIT` are finished, everything
  else including an unreadable code is abnormal and still restarts, because a spurious restart
  costs less than silently abandoning a crashed session. A finished session reports a new
  `finished` phase and health rather than the supervisor's `failed`, and it spends nothing from the
  restart budget, so a later crash still gets its full five attempts.

- Upgrading over an existing install no longer fails on the running daemon. Tauri's NSIS template
  stops the main binary and nothing else, and this app's daemon is deliberately designed to outlive
  its window — so at upgrade time it was still running, still holding its named pipe, and still
  holding an open image section on the executable the installer was about to overwrite. Every
  existing user took that path. An `NSIS_HOOK_PREINSTALL` now asks the daemon to shut down cleanly
  through the installed probe, so its session store is flushed rather than losing whatever
  accumulated since the last write, then stops both sidecars before the first file is written. The
  uninstaller does the same, for the same reason. The reboot fallback was never available here:
  `/REBOOTOK` applies to deletions only, and the delayed move it maps to needs Administrators while
  this installer runs as the current user.

- The release gate now proves the upgrade path rather than only a clean install. It installs, starts
  the app, closes the window while leaving the daemon running — the documented steady state — then
  installs over it and asserts the previous daemon was stopped, every sidecar survived, the app
  starts again and the session store was not quarantined. Verified to fail without the installer
  hook above. `-PreviousInstaller` stages a genuine previous-release-to-current upgrade when an
  earlier package is available; without it the same package is installed over itself, which holds
  the identical lock.

## [0.6.0] — 2026-08-03

### Added

- A comprehension pass over the fleet screen. The empty state now points at registering a project
  root rather than only offering to launch one session, and `?` in the toolbar opens a short
  explainer of the thing nobody guesses from looking at the screen: a row is not a terminal, which
  is exactly what lets thirty sessions share one. Its list of states is generated from the same
  table the rows are drawn from, so a status added later cannot appear on a row while missing from
  the explanation. Every status and the dwell timer carry a plain-language description, and a test
  now asserts that every icon-only control — in the markup and in the rows built at runtime —
  carries both a tooltip and an accessible name.

- A stored prompt library and a work queue that runs one prompt across many projects, creating a
  session per project as the fleet has room. Distinct from broadcast, which targets sessions that
  already exist; this one creates them, so it is far more careful. A repository with uncommitted
  changes is flagged rather than launched into — an agent let loose on a dirty tree mixes its work
  with the operator's, and the resulting diff cannot be separated afterwards — and a tree Git
  cannot read counts as not clean, never as clean. The check runs when the entry is about to
  start, not when the run was created, so a tree cleaned up in the meantime is not still flagged
  from an hour ago. Admission stays the fleet's decision: the queue asks for one slot at a time and
  stops when the answer is no. Every outcome category is reported, including the ones that did
  nothing, because a run over forty projects that says only "done" is one the operator has to audit
  by hand. Runs survive a restart.
- The prompt is delivered as a bracketed-paste pty write, not as a command-line argument: the
  session is launched with no initial prompt and the text goes onto its prompt queue. These
  prompts are kilobytes of prose, and a command line is the wrong place for that on any platform —
  an impossible one on Windows, where quoting mangles `&`, `^`, `|` and `%`.
- The library seeds itself once from the operator's own templates in `~/.claude/prompts` when they
  are present. Nothing is invented when they are not: a stored prompt named "drain the roadmap"
  containing something this app made up would be worse than an empty library. A seeded prompt the
  operator deletes stays deleted.
- Presets and registered project roots can now be removed from the UI, and the prompt library is
  reachable from the fleet toolbar for creating, editing, and deleting stored prompts.

- A prompt queue per session. Queue what the agent should do next while it is still working, and
  each prompt is sent when the run finishes — a session becomes something you load up and leave
  rather than something you come back to. The queue advances on the same reported status the fleet
  row is drawn from, never on a timer: a timer would fire into the middle of a long tool call,
  where the prompt is ignored or read as an answer to something else. A run that ends waiting for
  a permission decision or asking a question pauses the queue instead of answering blind, and says
  which of the two it is. Only one prompt is in flight at a time — writing to the pty does not
  change a session's status, so without a hold between "sent" and "picked up" the whole queue
  would fire in one burst. Prompts are addressed by id rather than position, since the operator is
  the one reordering them, and can be edited or withdrawn until they fire; an action that raced a
  fired prompt is reported rather than silently doing nothing. Queues survive a daemon restart and
  come back paused, because a restored session is not running. `≡` on each row opens it, and
  `terminalai-probe queue <id> [add <text>|pause|resume]` drives it from the command line.

- Queued and broadcast prompts now hold when the focused terminal has unsubmitted keyboard input.
  The row and queue name the state "focused and edited"; defocusing or explicitly submitting the
  input releases the transient hold, while broadcasts report the refusal separately from delivered
  sessions.

- A Projects view showing which known projects still have roadmap work: open items, how long ago
  the roadmap was touched, and the next unchecked item, sorted by most work first. Two states are
  kept distinct from zero throughout, because both would otherwise sort beside a finished project
  and drop out of consideration: a project with no `ROADMAP.md` is *unknown*, and a roadmap
  written as prose rather than checkboxes is *unreadable* — not empty. On the machine this was
  built against, 184 of 318 repositories fall into that second case, so treating it as zero would
  have reported 184 projects as finished. Checklist items inside fenced code blocks are skipped,
  so a roadmap documenting its own format does not count its examples as real work. Staleness
  comes from the file's modification time rather than a `git log` per project, which would be a
  process per project for a number that only needs to be approximate. Launching from a row carries
  that project's folder into the launcher.

- Register a root that holds your repositories — `~/repos` — and every Git repository under it
  becomes a launch target, so starting a session no longer means browsing to a folder you have
  visited a hundred times. The list is re-discovered on every launcher open rather than cached: a
  repository cloned five minutes ago is launchable without telling the app, and one deleted last
  week stops being offered. Discovery stops at a repository rather than descending into it, since
  otherwise every submodule and vendored dependency becomes a project of its own and a list of
  thirty becomes a list of four hundred; heavy directories are skipped and depth is bounded, so a
  root registered by mistake cannot turn into a full tree walk. Registering reports how many
  projects it found, because "registered" alone cannot distinguish a working root from one pointed
  at the wrong directory. A root already covered by another is refused, and a broader one replaces
  the roots it covers. Measured against a real 318-repository tree.

- The launcher ships with built-in presets, so a fresh install offers something to pick instead of
  an empty dropdown that asks the operator to invent a configuration before they know which ones
  matter. Six span the axes actually decided between — which agent, and how much rope it gets —
  each with a one-line description rather than four words of jargon. The preset that never asks
  permission is also the one that runs in its own worktree; shipping the dangerous half of that
  pair without the safe half would be the app recommending it. Built-ins are marked in the
  dropdown, cannot be edited in place — an edited copy would silently outlive a corrected version
  in a later release, so saving under a built-in's name is refused with the suggestion to clone —
  and are hidden rather than deleted, because a preset that exists only in code cannot be
  recreated by hand. `↺` beside the preset list offers every hidden built-in again.
- Presets saved before this version still load: the store read a bare JSON array and now reads
  either shape, rewriting in the current one on the next save.

### Fixed

- The unaudited-surface pass now has explicit evidence for the VT/grid, MCP, runtime-capability,
  app-server, and installer-build surfaces; non-Windows execution, live-agent round trips, fresh
  load measurements, and installer execution remain tracked as named blocked evidence rather than
  being implied by passing unit tests.
- New Claude launches receive a generated UUID through `--session-id`; transcript tailing follows
  that named JSONL file directly and leaves timestamp ranking only for agents without the explicit
  binding capability.
- Preflight now reads local Claude managed policy sources and marks installed hooks as blocked and
  non-fixable when an administrator disables them or allows only managed/plugin hooks.
- Daemon accept retries now back off after persistent listener errors, the optional Codex app-server
  transport rejects oversized frames, and session-store quarantine moves cannot replace a same-name
  file. App and daemon logs now have distinct prefixes, while the app's notification diagnostic no
  longer contains a garbled spacing run.
- The Tauri bridge no longer serializes per-tool-call agent events that the renderer discards, and
  the unreachable output-event branch is gone because terminal bytes already use the dedicated
  channel. Session cost labels now share the rollup's below-one-cent formatting, and all renderer
  timestamp consumers decode serde objects plus numeric seconds or milliseconds consistently.
- Cargo-deny now checks the workspace's explicit license allowlist, duplicate-version policy, wildcard
  dependencies, and registry/source provenance in addition to advisories.
- Land, external-session discovery, work-run admission, preflight probing, and project discovery now
  run through Tauri's blocking executor, so daemon waits, CLI probes, and repository walks do not
  occupy the window's command thread. The store handles are cloned into those tasks and retain their
  mutex across each load-modify-save operation.

- The pty's headless cursor-position fallback now answers every startup query
  in a burst exactly once, then stops when the renderer is attached or the
  startup handshake has been answered. This prevents the focused xterm from
  receiving a second, incorrect `1;1` response.
- External-session discovery now distinguishes an unavailable registry from a
  readable empty one, and drains large CLI responses while enforcing its
  timeout, avoiding repeated spawns and pipe deadlocks.
- Folder-picker failures now reach the existing toast path for launcher,
  registered-root, and extra-directory selection instead of leaving dead
  buttons through unhandled promise rejections.
- Launcher previews now carry a request generation, so a slower response for an
  older folder or agent choice cannot overwrite the current command preview.
- Focus switches now serialize output-channel registration and reattach the prior
  session after a failed switch, so rapid navigation cannot leave the pane blank
  or restore an older focus over a newer one.
- Refreshes now serialize snapshot requests and replay session events received
  while the snapshot was in flight, so a refresh cannot briefly roll attention
  or removal state back to an older view.
- Hook parsing now takes a caller-owned working-directory fallback: CLI hooks
  keep their agent directory, while HTTP hooks leave `cwd` absent instead of
  inheriting the daemon's directory.
- Fleet controls now describe their current action state: Wide can hide the
  extra columns, grouping says that it cycles, and the umbrella filter says
  “Needs attention.” Neutral toasts no longer look like errors.
- Narrow fleet rows now reveal their status label, the empty state waits for
  the initial snapshot instead of overlapping its spinner, and the first column
  header follows the compact/Wide fields actually on screen.

- Persistent attention states suppressed during startup or long-tool grace are now rechecked and
  raised when the grace period ends, even without another status transition.
- Scrollback cleanup now waits for a full writer queue instead of losing a session's delete
  request and leaving its disk segments orphaned.
- Session ids now escape underscores in both scrollback logs and store sidecars, preventing
  restored sessions with colliding names from sharing history.
- A damaged persisted scrollback sidecar now clears only that session's replay history while the
  remaining session store still loads.
- Land requests now refuse expected Git hashes shorter than four characters, avoiding accidental
  matches against a target the operator did not review.
- Land verification budgets are now capped before they reach the process deadline, so an absurd
  wire value cannot overflow `Instant` and kill the landing request thread.
- Corrupt Claude registry timestamps now skip only the invalid external session instead of
  overflowing the system clock and panicking the reader.
- The web update fallback now uses the package manifest version injected by Vite, so failed native
  version lookups cannot report a stale release.
- The web shell now applies Fluent placeholders, routes update and action feedback through the
  catalog, and checks that every non-dynamic catalog message and localization attribute has a
  renderer reference.

- The launcher now names its dialog for assistive technology and keeps empty-folder
  validation beside the field with focus, aria-invalid and a correctly rendered Windows path placeholder.
- Template and lease relative-path validation now rejects every non-normal path component,
  including Windows root-relative and drive-relative paths that could escape the repository.
- Review landing refusals now stay in the review entry as an accessible error, preserving
  the full reason after the transient action feedback has disappeared.
- Fleet summary and diagnostics updates now avoid unnecessary DOM replacement, preserving
  keyboard focus and text selection across the one-second status tick.
- Fleet and queue row actions now use 24×24px hit boxes while preserving the compact fleet row pitch.

- The Presets selector now keeps a visible keyboard focus indicator in both themes.

- Project discovery, prompt queue and review failures now stay visible as explicit error panels with
  escaped details and Retry controls instead of masquerading as empty results.

- The installer gate now requires a newly running `terminalai-daemon.exe` from the scratch prefix as
  well as its control pipe, so a pre-existing daemon cannot make the installed-app check pass.

- Focused terminal reattachment now gates live output behind the replay and drops the already-replayed
  suffix, preventing duplicated or out-of-order bytes during attach.

- A transient client-thread allocation failure now drops only that connection
  instead of invoking a second panic-on-spawn fallback that could kill the daemon.

- Daemon shutdown now stops and joins the store bridge, persists one final
  post-teardown snapshot synchronously, and waits for teardown during Windows
  console close, logoff and shutdown events.

- Explainer, queue, project, broadcast and rollup dialogs now inset their
  headings and content to the same horizontal boundary as the action footer.

- Rate-limited and preflight red/green states now keep their declared glyph colors and backgrounds,
  including the red pulse shown for a focused rate-limited session.

- Grouping now reconciles each row's group chip during keyed updates, so
  enabling grouping adds labels to existing rows and status/folder changes do
  not leave stale chips behind.
- The broadcast dialog now remembers which eligible sessions the operator left
  checked when a partial refusal forces the target list to re-render, so it no
  longer silently re-ticks excluded sessions.
- Repository database leases now validate their environment-variable names:
  administrative URLs must use an operator-controlled `TERMINALAI_*` name, and
  session URLs cannot shadow the sanitized process baseline or TerminalAI's
  own session variables.
- Repository lease commands now use the sanitized child environment and carry
  Postgres connection strings in `PGDATABASE` rather than argv. `PGURI` is no
  longer emitted, so database passwords do not leak through process listings or
  the daemon's inherited environment.
- Hook delivery is now bound to a random per-session secret carried only in the
  supervised agent environment. Managed Claude hooks use the command adapter to
  carry it, explicit HTTP callers must supply it in addition to the daemon bearer,
  cwd is no longer an identity fallback, and an event cannot overwrite a bound
  native resume id. The control protocol advances to v3 for the authenticated
  hook request shape.
- The loopback HTTP hook listener now uses four bounded workers and a bounded connection queue,
  catches worker panics, and enforces a five-second request deadline alongside per-read timeouts.
  A stalled-client regression proves a second authenticated hook still receives a 202 response.
- Scrollback gaps are now tracked per session, disk append failures are retained as pending gaps,
  and rate-limited warnings explain when the durable log falls behind. Markers are emitted into the
  session that lost the bytes, with regressions covering cross-session attribution and recovery.
- Transcript polling now keeps its readers behind a separate mutex, so file reads and recursive
  transcript discovery cannot hold the fleet state lock while PTY output and hook events arrive.
- Claude HTTP hooks now downgrade to the existing command transport when the app shuts down, so
  the dead daemon URL, loopback allowlist entry, and bearer token do not persist in global settings.
  The shutdown-path regression preserves the managed fail-open hook without retaining the secret.
- “Load older output” now requests the in-memory ring plus a bounded older window, and the daemon
  frame/history budgets carry that full replay. A >1 MiB registry regression proves the returned
  history contains bytes the ring has already dropped.
- Terminal resize deduplication now includes the focused session id, so switching sessions sends
  the current renderer dimensions to the newly focused pty even when the grid size is unchanged.
  A sizing regression test keeps the per-session signature intact.
- Light mode now overrides the fleet-row interaction state, external-session strip, pinned panes,
  and filters with the light palette. Their shared rules sit before the theme block, and contrast
  tests keep the variable-based surfaces above 4.5:1 instead of letting dark rgba backgrounds win.
- The Review view's stylesheet now contains a real `.review-view` rule instead of a literal `\n`
  selector escape, restoring its fixed height, internal scrolling, and panel padding. A CSSOM test
  locks the selector and all three layout declarations together.
- Codex transcript discovery now reads each rollout's first `session_meta` record and binds only
  to a rollout whose declared working directory matches the session. Incomplete metadata is
  deferred, concurrent projects cannot cross-adopt cost or resume ids, and transcript binding
  goes through the tail's reset-aware `follow` path.
- Resume IDs from hooks and transcripts are now validated before storage and again before argv
  construction, so flag-like values cannot alter a revived Claude or Codex launch.
- Environment leases now reject escaping compose files, canonicalize accepted paths inside the
  repository before teardown, and ignore repository attempts to enable destructive volume removal.
- Work runs now mark exited or removed sessions as done and automatically admit
  the next pending project when a fleet slot opens.
- Transcript tailing now advances past oversized and invalid-UTF-8 JSONL records instead of
  retrying the same offset forever. The bounded byte reader preserves partial-line behavior while
  skipping records the JSON parser cannot safely consume.
- Release `terminalai.exe` now uses the Windows GUI subsystem, so Explorer and Start-Menu launches
  do not allocate a companion console. The installer gate reads the installed PE header and fails
  unless it reports subsystem 2.
- The Preflight and Review views now open correctly. Their visibility synchronizers reference the
  column-label element by an id that exists in the shell, with a DOM regression test covering every
  element named by both synchronizers.
- All 55 Tauri commands now have grants in every desktop capability profile. The build checks both
  the `generate_handler!` registration and the capability files, so a newly registered command
  cannot compile into an unusable invoke path or leave a stale permission behind.
- Applying a preset that names no working directory no longer blanks the folder. Which
  configuration to use and which project to run it on are separate choices.

## [0.5.0] — 2026-08-03

### Added

- Repositories can declare their own launch templates in `.terminalai/templates.toml`, versioned
  with the code they describe, so starting work on a familiar project does not begin by
  remembering which permission mode and effort level it wants. Templates appear in the launcher
  when that folder is chosen and are re-read on every change, because pulling a branch that edits
  the file should change what the launcher offers. A template may only set choices the launcher
  already models: `extra_args` and `cwd` are refused at the schema, since the file arrives with a
  clone and one that could put arbitrary text on an agent's command line would be argument
  injection; extra writable directories are refused if they escape the repository; and an
  unrecognized value for agent, effort, permission or sandbox is a refusal rather than a string
  passed through to the CLI. A malformed file is reported, never treated as no templates. This
  repository ships its own, which is also what the format's test reads.

- Broadcast one prompt to several sessions, from a dialog in the fleet toolbar or with
  `terminalai-probe broadcast <id>... -- <text>`. The result is reported per session, never as one
  status: a broadcast that says only "sent" leaves the operator unable to tell which agents got the
  prompt, and re-sending to find out delivers it twice to the ones that already had it. A session
  waiting on a permission decision is refused by name — a permission prompt is a specific question
  with a small set of valid answers, and prompt text answers something, just not what was meant —
  while a session merely asking a question does receive it. Ineligible sessions are listed in the
  dialog with their reason rather than hidden, the selection is re-checked at send time because a
  session can enter a permission prompt while the operator is typing, and the CLI exits non-zero if
  anything was refused.

- Cost and token rollup across the fleet. The header's spend figure is now a control that opens a
  breakdown by agent, by project folder, and by session, each with its own token totals — cost and
  tokens answer different questions, and a run heavy in cache reads costs about what one heavy in
  output does while behaving nothing alike. `Session.tokens` carries the transcript's totals
  alongside the price they produced. Every grouping states its own coverage: a session whose
  transcript has not been read is counted apart and shown as an em dash, never folded in as zero,
  because zero is the claim that a session ran and spent nothing — and treating "unknown" as zero
  makes the total quietly too low exactly when someone is checking whether it is too high. A cost
  under a cent reads `<$0.01` rather than rounding to `$0.00`, which would be indistinguishable
  from a session that has not started spending.

- A private Git worktree per session, requested from the launcher and cleaned up with the row.
  Two agents on one repository was the fleet's most obvious use and its worst failure: one
  session's uncommitted edit became another's unexplained diff. Each isolated session gets its own
  checkout on its own `terminalai/<id>` branch, cut outside the repository so it is not untracked
  clutter in the parent's status. Nothing is ever reused — an existing branch or directory is
  refused rather than adopted, because neither this code nor the operator can tell leftovers from
  unlanded work. Removal takes the checkout but offers the branch to `git branch -d`, so a branch
  holding unmerged commits is kept and reported instead of deleted; a checkout deleted behind
  Git's back is repaired by pruning rather than left as a registration that breaks every later
  add. Repository leases now copy their untracked config from the repository the checkout was cut
  from, which is what makes an isolated session runnable at all. `--worktree` on
  `terminalai-probe start`.

- Scrollback to disk, under the bounded in-memory ring. Each session appends to a rotating pair of
  segments capped at 8 MB total, so history outlives the 512 KB the ring can hold, and survives a
  daemon restart. The bound is in bytes rather than lines because a line costs whatever the pane is
  wide — the same "10,000 lines" is three times the storage at 360 columns as at 120. Writes are
  queued to a dedicated thread: output arrives on the pty reader with the registry's state lock
  held, and a blocking write there would stall every other session and back-pressure the agent that
  produced the bytes. If that queue ever fills, the bytes it drops are announced in place in the
  log rather than silently omitted. `⇡` in the terminal toolbar loads older output into the focused
  pane, and `terminalai-probe history <id> [bytes]` reads it from the CLI.

### Changed

- The session store no longer carries session output. The log is now the durable copy, so the
  store's debounced rewrite stopped copying every session's whole ring — up to 30 × 512 KB as often
  as once a second — and a restarted daemon replays its panes from the log instead.

### Fixed

- A session no longer adopts the transcript of another session running in the same folder.
  Discovery ranked candidates by modification time, so a run that was already in progress — and
  therefore being appended to right now — always looked newer than the file the new session was
  waiting for, and its cost, token totals and resume id were reported against the new row.
  Candidates are now ranked and floored by creation time, the only stamp that says which run a file
  belongs to, with a 100 ms grace below the floor for the coarse system clock that stamps files.
  Ties are broken by path so the same directory always yields the same answer.

## [0.4.0] — 2026-08-03

### Added

- Filter and group the fleet list. Agent and status dropdowns sit alongside the free-text box —
  text matches anything on a row, while these are exact dimensions an operator thinks in. A group
  button cycles none → folder → agent → status and always states the mode it is in. Grouping
  reorders so a group's members are adjacent and labels each row, rather than inserting headers:
  the list is an ARIA listbox, and a child that is not an option would break its semantics and its
  keyboard model. A group's position follows its most urgent member, so the folder holding a
  session that needs you stays at the top.
- Windows toasts for sessions that want the operator. A session entering `NeedsYou`,
  `NeedsApproval` or `AwaitingInput` raises a toast naming it, why it is blocked, and which repo it
  is in — a toast reading only "Needs you" is useless with thirty sessions. Clicking it focuses
  that session and raises the window, using the in-process `on_activated` handler rather than a COM
  activator: the app is by definition already running when it raises the toast. A rate-limited
  session deliberately does not toast — it is blocked, but there is nothing the operator can do.
  Toasts are short-duration because an unpackaged process cannot reliably withdraw one from the
  action centre, and a toast outliving the state it describes sends the operator to a session that
  has moved on. A missing Start Menu shortcut makes every toast silently fail, so that is reported
  once rather than per event.
- Split view for pinned sessions. Up to three pinned sessions now render live panes beneath the
  focused terminal, drawn from the Rust-side grids the daemon already kept for them rather than
  from more xterm instances — one renderer is what lets the fleet hold ~29 rows, and three more
  would undo that. Panes are reconciled by session id so one snapshot cannot blank a sibling, a
  pane with no snapshot yet says so rather than showing an empty box, and unpinning drops the
  stored grid so repinning cannot show a stale frame. `terminalai-probe pin` and
  `terminalai-probe grid` drive the same path from the command line.
- Transcript tailing. Each live session's JSONL is followed incrementally — only the bytes appended
  since the last poll — for the three things the pty cannot carry: the agent's own session id (what
  `--resume` takes), the last thing it actually said, and what the run cost. `Session.cost_usd` and
  the fleet's aggregate are now real numbers instead of a computed-looking zero; measured live at
  $0.98 for one session against its own transcript. A half-written record is left for the next poll,
  a truncated file restarts rather than splicing two records together, and a deleted one is
  rediscovered. Only `text` blocks become the row label, so a tool call's arguments never do.
- `Session.last_message` carries the transcript's text and the row prefers it over `last_line`,
  which is the tail of a rendered TUI and carries whatever escape sequences the last redraw left.

- `terminalai-probe mcp` exposes the fleet to an MCP client over stdio. Read tools — list sessions
  (both supervised and external), read one session's status, read the tail of its terminal output,
  read fleet cost — are ungated. Mutating tools require *both* an out-of-band write token and a
  session opted in when the server starts, are not advertised at all unless enabled, and every
  attempt is logged whether it was allowed or refused. Session transcripts are never exposed and
  the session summary is a whitelist, so a field added to `Session` later cannot leak through a
  tool that was reviewed once. Tool metadata is compile-time constant — asserted by comparing the
  advertised tools across two different fleets — because tool poisoning is only possible when
  metadata comes from somewhere mutable. Terminal output is stripped of escape sequences before it
  leaves the process.

### Fixed

- A session launched into a folder that already held transcripts bound to the newest existing one
  on its first poll, reporting an earlier run's cost, token totals and resume id as its own —
  observed live before the fix. Discovery now ignores transcripts older than the session.
- Records without a `requestId` were summed. Codex reports the session's *cumulative* usage on
  every turn, so summing multiplied the real figure by the number of turns; a cumulative record now
  replaces rather than accumulates, and never walks backwards.

## [0.3.0] — 2026-08-03

### Added

- Repositories can declare a per-session environment lease in `.terminalai/environment.toml`, so it
  is versioned with the code it describes. Beyond the deterministic port block that already existed,
  a lease covers the three things parallel sessions actually collide on: untracked config copied in
  by glob, a docker compose project prefix so two sessions build two stacks, and a Postgres database
  cloned per session with `CREATE DATABASE … TEMPLATE …`. Leases are released when the session tears
  down, and the raw setup/teardown script escape hatch is unchanged. Depth over generality is
  deliberate — a generic hook API is what every other tool already tells the operator to write.
  A lease that cannot be read, escapes the repository, or names a database that is not a plain
  identifier refuses the launch rather than being ignored, because a session that quietly falls back
  to the shared database is the exact collision the lease exists to prevent.
- A session's work can now be landed from the review surface, through a gate that refuses rather
  than half-applies. Landings are serialised daemon-wide, so a request that waited in the queue is
  checked against a fresh read of the target rather than against what the review showed. A landing
  is refused whole — naming the one specific condition — when the target moved since review, the
  target tree is dirty, conflict markers are present, the patch no longer applies, or a configured
  verify command fails; a failed verify reverses the patch it just applied, and the one case where
  that reversal itself fails is reported as a mixed state needing manual repair rather than folded
  into a generic error. Nothing is merged, staged, committed, or auto-resolved on the operator's
  behalf, and the landed change is left uncommitted because committing is their decision.
  `terminalai-probe land` drives the same path from the command line.
- `ReviewItem` now carries `target_head`, the commit a review was read against. The moved-target
  refusal compares against it; without it that check would have had nothing to compare.
- Rate limiting is now a first-class row state. A session a provider is refusing renders as
  `Rate limited` with which quota tripped and when it reopens, sorts with the attention states
  rather than with the busy ones, and releases its admission slot so a queued session can take it.
  The fleet header counts limited sessions and shows the soonest reset across the fleet. The state
  is only ever entered from an explicit agent report — Codex's `rate_limits` table (the
  most-consumed window wins) or a Claude retry carrying a `rate_limit`/`overloaded` category — never
  from a session going quiet, which is indistinguishable from a long tool call. A missing reset time
  is said out loud rather than guessed, and a plain transport error is not treated as a quota.
  Verified end to end against a running daemon: a `weekly` window at 100% won over `primary` at 31%,
  live sessions went 1 → 0, and a later report with room left returned the row to the fleet.
- The focused pane now runs the WebGL renderer. xterm 6.0 removed `addon-canvas`, so with no WebGL
  addon the DOM renderer — the slowest of the three — was the only one available. Context creation,
  addon construction, and later context loss each fall back to the DOM renderer rather than blanking
  the pane. Measured in a Chromium engine: 2 canvases attached, renderer reports loaded.
- OSC 8 hyperlinks emitted by a session are now clickable. They already reached the pane, because
  the focused renderer replays raw PTY bytes, but without a link handler xterm underlined them and
  clicking did nothing. The URI is agent-controlled, so it is opened only after Rust accepts the
  scheme: `http`, `https` and `mailto` only, control characters refused, and every refusal reported.


- Added Windows process-hygiene controls: background ConPTY sessions use reversible EcoQoS and
  memory-priority settings, focus and pin changes restore normal priority, waiting-session counts
  render as a numeric taskbar overlay, and `terminalai-probe hygiene` publishes repeatable console
  churn and input-latency measurements.
- Added one shared Fluent catalog at `web/src/i18n/terminalai.ftl`, loaded and validated by the
  Rust daemon and formatted by the web renderer. Status diagnostics now carry structured reason
  kinds with arguments, while localized counts and dwell labels use `Intl.PluralRules` and
  `Intl.RelativeTimeFormat` without expanding the compact fleet row.
- Expanded Claude/Codex hook coverage across prompt, tool failure, permission, subagent,
  compaction and session-end events. Claude uses a daemon-lifetime bearer-authenticated loopback
  HTTP endpoint with an exact Host allowlist and Origin rejection; installation falls back to the
  existing fail-open command adapter when HTTP cannot be configured. Unknown event names remain
  visible in diagnostics instead of being discarded.
- Replaced static launcher model and effort lists with cached runtime capability probes. Codex uses
  `model/list` plus `codex features list`; Claude reads `system/init`. Per-model effort order is
  preserved, values are invalidated when the resolved binary version changes, and unknown user
  values remain launchable with warnings.
- `SessionRegistry` now supervises sessions through an object-safe `AgentDomain`/`AgentSession`
  contract. `LocalPtyDomain` keeps the existing ConPTY behavior as the default while a future
  remote relay can supply input, output, resize and lifecycle operations without local handles.
- Session persistence now carries a `TerminalAI.session-store` magic string and schema 2. Legacy
  schema 0 and 1 files migrate atomically after a `sessions.v<old>.bak` copy, future versions are
  still refused, and unknown top-level fields survive through `serde(flatten)` and registry writes.
- The contrast audit now keeps small text on WCAG-AA tokens in both dark and OS light palettes,
  uses `surface0` for selected rows, and treats overlay/surface tokens as decoration only. The
  focused terminal has an opt-in xterm screen-reader toggle with its right-click behavior called
  out in the control label.
- Forced-colors mode maps fleet status, controls and the xterm surface to system colors while
  keeping `forced-color-adjust: none` limited to xterm. Reduced-motion mode also stops the fleet
  spinner and status glow effects.
- Added structured daemon and desktop-shell logging under `%LOCALAPPDATA%\TerminalAI\logs\`
  with fourteen daily files, a bounded in-app log panel, per-session tracing fields, and panic
  records that drain through the process-held worker guard.
- The Windows pipe ACL now grants the current interactive user's explicit SID
  (plus `SYSTEM`) instead of the elevation-sensitive owner-rights alias. PID
  mismatches are reported as diagnostics, and setup/teardown hooks are clearly
  documented as opt-in local shell execution.
- The daemon now has a graceful `terminalai-probe shutdown` control request, Windows console
  teardown handling, and a single-instance binding guard. Protocol skew reports the running daemon
  PID and a concrete stop command; the desktop shell refuses to spawn a second daemon in that case.
- The control endpoint now has a stable name with a legacy v2 fallback, so protocol negotiation can
  detect upgrades without stranding a daemon that still owns live sessions.

### Changed

- xterm now measures character widths with the unicode11 addon. It defaulted to Unicode 6 (confirmed
  by reading `unicode.activeVersion` in a real browser) while the Rust grid uses `unicode-width`
  against a modern table, so the two disagreed about where a line wraps and the status inferred from
  the Rust grid could stop describing what the pane showed.

- Agent resolution now runs against an injectable filesystem (`which` 8's `Sys` trait), so the npm
  prefix and `PATH` routes are covered by tests instead of by whatever happens to be installed on
  the machine running the suite. Eight cases are now exercised directly, including an unpopulated
  Windows `PATHEXT` — harmless only because the query carries an explicit `.exe`, which is now
  asserted rather than assumed. Non-fatal search errors are logged instead of collapsing into an
  indistinguishable "not installed".
- Updated `toml_edit` 0.20.2 → 0.25 (`Document` → `DocumentMut`). Release binaries shrank: daemon
  2,214,912 → 2,192,896 bytes, probe 1,693,184 → 1,643,008 bytes. Note the lockfile still carries
  three `toml_edit` copies, but only one was ever compiled into a Windows binary — the other two
  reach the lockfile through `glib-macros` (Linux GTK) and `num_enum_derive` (Android) and are
  excluded by target. The direct dependency now shares the 0.25 copy rather than adding a fourth.

### Fixed

- Environment teardown failures during a failed launch were discarded with `let _`, so a leaked
  database or a compose stack left running was indistinguishable from a clean unwind. They are now
  logged with the session and the cause. Teardown also reports *every* failure rather than the
  first, since a container left running matters even when the database also failed to drop.
- A session that opened a synchronized update (DEC 2026) and then stopped writing — killed
  mid-frame, or with its write truncated — froze that session's terminal grid permanently. `vte`
  buffers everything between `ESC[?2026h` and `ESC[?2026l`, and arms a 150ms deadline when the
  update opens, but reports only that a deadline *exists*, never that it expired, so the buffer was
  never flushed and every status inferred from the grid went quietly stale. Expiry is the caller's
  job and now happens on both the write and the read side.

## [0.2.0] — 2026-08-03

### Added

- `terminalai-core::registry` — Rust-owned session lifetime, sorted fleet snapshots, pushed
  output/status events, focus/read/pin controls, and bounded per-session scrollback.
- `terminalai-app` — Tauri 2.11.5/WebView2 desktop shell with a Catppuccin Mocha fleet list,
  28px status rows, launcher dialog, exact command preview, folder pickers and saved presets.
- One canonical 120×40 xterm renderer that replays focused-session scrollback and only resizes on
  the explicit terminal reset action.
- Normalized Claude/Codex hook ingress through the authenticated daemon pipe, with non-blocking
  `terminalai-probe hook` delivery and separate approval/input attention states.
- `terminalai_core::transcript::TranscriptAccumulator` — nested usage extraction, requestId
  deduplication, and cost calculation through a caller-pinned pricing-table version.
- Explicit session supervision state now separates agent phase from process health, exposes PID,
  resume id, exit metadata and restart counters, and retries unexpected exits with exponential
  backoff before entering a terminal failed state.
- Review aggregation now uses a bounded worker pool with per-repository Git deadlines, capped
  incremental output capture, process-tree cleanup on Windows timeouts and an explicit timed-out
  partial-result state.
- Reversible agent hook configuration now previews, installs, reports and removes only
  `--terminalai-managed` Claude JSON and Codex TOML entries; Claude handlers are asynchronous,
  Codex preserves unrelated `notify` commands, and the installed app hook path fails open when the
  daemon is unavailable.
- The fleet list now exposes distinct glyphs and visible labels for every status, keyboard row
  activation, screen-reader announcements, light/dark contrast tokens, forced-colors support and
  reduced-motion behavior.
- Fleet navigation now exposes a single-select listbox with roving keyboard focus, option position
  metadata, explicit selection state and real per-row action buttons.
- Fleet status announcements now coalesce actionable transitions for two seconds, while sorting
  pauses during list interaction and exposes an Apply control for pending priority moves.
- The desktop shell now opens even when the daemon is unavailable and presents a first-run
  preflight panel for agent versions, managed hooks, daemon reachability and the Start-Menu app ID,
  with local Fix and Recheck actions that remain reachable from Diagnostics.
- Release checks now use cargo-deny against the Windows target, denying unreviewed vulnerabilities
  and documenting the five currently unavoidable transitive Unicode advisory exceptions with
  upstream RustSec links.
- `terminalai-core::grid::TerminalGrid` now parses each session's ANSI output into a bounded Rust
  grid with cursor motion, scrolling, alternate-screen restore and split UTF-8 coverage. Only the
  focused session (and future pinned panes) receives live output events; background sessions keep
  parsed state without creating more xterm instances.
- R-14 restore actions now reattach live rows with bounded replay, revive stopped rows through
  Claude/Codex native resume ids, or archive layout/cwd/command metadata. A debounced daemon worker
  persists the versioned JSON store without automatically restarting recovered agents.
- Corrupt, truncated, and unsupported-version session stores are quarantined with a timestamped
  filename so the daemon starts with an empty fleet and the desktop shell names the saved path in a
  dismissible banner.
- Fleet ordering now compares persisted status-transition timestamps and stable session ids, so a
  snapshot cannot become non-transitive while it is being sorted.
- Fleet rows now reconcile by session id, mutating existing row content while preserving focused
  reply inputs and their caret across live updates and filtered views; frontend coverage runs with
  `npm --prefix web test`.
- PTY output and replay now stay as raw bytes from the registry through the daemon and Tauri
  `Raw` channel, with 12 ms batching so split UTF-8 sequences reach xterm intact.
- Windows PTY sessions now belong to kill-on-close job objects, so manual kills and session drops
  reap descendants; daemon teardown runs each active session hook once and releases its port block.
- The PTY environment now preserves the Windows user/runtime directories, processor count and
  present HTTP(S)/no-proxy settings; installed Claude and Codex binaries are covered by sanitized
  `--version` launch tests while parent-only secrets remain excluded.
- Session persistence now wakes a worker with a lightweight signal; it builds the registry snapshot
  after the debounce and stores replay tails as per-session binary sidecars instead of cloning and
  JSON-encoding a full ring for every output event.
- Persistence now writes after a quiet period or a one-second maximum interval, even under
  continuous output, and always snapshots the latest registry state.
- Row summaries now scan a bounded tail of each ring buffer instead of copying and decoding the
  full 512 KiB scrollback for every output chunk.
- Background grids now honor DECSTBM scroll margins, saved-line erase, terminal resize, custom tab
  stops, automatic wrapping, and Unicode wide-character cell widths, with recorded CLI reference
  streams and arbitrary-byte parser coverage.
- A poisoned registry lock is now recovered safely; the daemon keeps serving health and metadata
  requests while returning an explicit state-unavailable error for fleet mutations and snapshots.
- Notification retraction is now session-wide across attention-state changes and archive/kill/remove
  paths, preventing stale badges and toasts.
- Automatic restarts now share one bounded timer scheduler; admission-blocked attempts consume
  restart backoff and budget, and scheduler failure marks the session failed instead of abandoning it.
- Environment setup and teardown hooks now run on dedicated workers with visible preparing and
  teardown phases; Windows hook timeouts terminate the entire descendant process tree.
- Registry and IPC event delivery now use bounded nonblocking queues with counted drops, and each
  subscribed connection stops and joins its event bridge before the writer thread is joined.
- R-16 adds lifecycle-aware attention notifications with stable dedup keys, repository grouping,
  automatic retraction on progress, and startup/long-tool grace suppression. The shell consumes
  raised notifications as deduplicated, click-to-focus in-app alerts.
- R-17 adds daemon-owned admission control with a configurable live-process cap, FIFO overflow
  queue, a Claude default spend cap, and a live/queued/aggregate-spend fleet summary.
- R-18 now treats failed process reconciliation as an explicit unknown state, retaining the row and
  PTY until a later positive exit result proves that the process is gone.
- R-19 adds an opt-in `codex-app-server` daemon transport. The core accepts Codex JSONL/JSON-RPC
  notifications, preserves unknown methods, maps status and approval signals into the fleet event
  model, and encodes steer/interrupt requests while the default daemon remains unchanged.
- R-20 promotes the headless probe into a daemon control client with JSON `list`, `start`, `stop`,
  `send`, and `status` commands. It uses the same named-pipe protocol as the GUI, returns one stable
  JSON object per invocation, and keeps connection/remote failures on a nonzero exit path.
- R-21 reshapes fleet rows around status glyphs, repository/branch, dwell, tool-progress fractions,
  restart counts and ellipsized output, with an all-state header strip, `/` filtering, and a Wide
  view for model, effort and reported cost.
- R-22 adds deterministic per-session service-port blocks, optional setup/teardown shell hooks,
  hook/agent environment variables, launcher and probe controls, and allocated-port row metadata.
- R-33 adds a daemon-owned Review view that aggregates Git diffs across sessions, ranks review cost,
  preserves conflict markers, and lets the operator mark a session reviewed.
- R-25 adds real-pty echo coverage for output and exit detection plus version-pinned Claude Code
  and Codex CLI launch-argument golden fixtures.
- R-26 adds bounded per-session status evidence, a focused "why this state" diagnostics view, and
  structured JSONL crash records for daemon or app panics.
- R-28 adds unsigned NSIS/MSI packaging with Evergreen WebView2 bootstrapper mode and a read-only
  GitHub release check in the shell; update checks never download or install an artifact.
- R-34 routes hook settings, session state and launcher presets through one fsync-backed atomic
  writer with advisory locking and `.bak` recovery copies.
- R-35 terminates launch options before positional prompts, rejects non-finite or negative Claude
  budgets, and documents the trusted-input-only extra-argument escape hatch.
- Claude/Codex hook payloads now share one parser, including session/thread id aliases and
  permission/idle notification normalization for approval and awaiting-input states.
- The fleet now has a needs-input filter and per-row reply controls that send bracketed paste
  through the daemon without switching terminal focus.
- Strict agent/binary matching before a process is spawned, preventing a launcher configuration
  from accidentally starting the wrong CLI.

### Fixed

- The core now passes strict clippy; `Agent` uses a derived default and fleet tests use arrays
  where allocation is unnecessary.
- PTY children now receive an explicit safe environment allowlist instead of inherited API keys,
  registry secrets and unrelated agent credentials.
- Poisoned PTY locks return `PtyError::Gone` rather than panicking the host process.
- Codex now emits its supported `max` reasoning effort and explicit `Plan` collaboration-mode
  config override.
- Documentation now distinguishes thirty tracked sessions from the much smaller live-process
  budget measured on the development machine.
- `terminalai-daemon` now owns the registry behind a versioned local-socket protocol with an
  explicit event subscription, peer-PID handshake, owner/SYSTEM Windows DACL and no client
  impersonation path.
- Resume variants now use an explicit JSON `id` field, and the unsigned NSIS/MSI release lane
  builds the Vite frontend and app bundle from the workspace root.
- The focused terminal is now visible: the placeholder is hidden the moment a session attaches
  and restored when focus clears, so the xterm renderer occupies the host box instead of being
  laid out below it.
- Closing the launcher, or pressing Enter in any launcher field, no longer spawns an agent —
  every dialog control is an explicit button and the form refuses submission outright.
- The focused terminal now fits its pane. The fit addon was constructed and registered but never
  called and there was no resize listener, so the grid stayed at a hard-coded 120x40 regardless of
  pane size. Measured in WebView2's engine: 99 columns at a 1356px window, 141 at 1920px, and back
  down when the panel narrows. Resizes are debounced by 180 ms and skipped when the size is
  unchanged, because agent TUIs hard-wrap and a resize arriving mid-drag corrupts the output the
  supervisor parses for status.
- A focus switch now carries a generation token, so output that arrives late for the session you
  just left is discarded instead of being written into the new session's grid.
- The control protocol enforces a 1 MiB frame limit on read, so a peer that sends bytes without a
  newline can no longer exhaust memory on either end. Oversized `Write` payloads are refused whole
  with a typed error rather than truncated — half a prompt reaching an agent is worse than none.
- A malformed control frame now returns an error and keeps the connection, instead of tearing it
  down with no reply, and a transient thread-spawn failure in the accept loop drops that one
  connection instead of ending `serve()` and abandoning every live agent.
- The daemon restricts library loading to System32 before any pseudo-console is created.
  `portable-pty` asks for `conpty.dll` by bare name and no such file exists in System32 on this
  OS build, so the search reached the application and working directories — both writable by a
  non-administrator — in the process that owns every supervised agent.
- The app's own commands are now covered by declared Tauri ACL policy rather than only a runtime
  patch: `build.rs` declares an `AppManifest` listing all 26 commands, and the capability drops
  from the nine sets `core:default` expands to down to the four actually used, marked local-only.
  A build assertion fails, naming the command, if one is added without a matching grant — the ACL
  is checked at invoke time, so that failure would otherwise reach a user instead of the build.
- The CSP now names `base-uri`, `form-action`, `frame-ancestors` and `object-src` explicitly.
  None of them inherit from `default-src`, so the previous policy left an injected `<base>` or
  `<form>` unconstrained.
- Every interpolation into a markup attribute goes through `escapeHtml`, and a test enforces it —
  agent output reaches those strings, and one unescaped attribute is enough to break out of it.
  Agent output also still cannot reach the system clipboard: the xterm OSC 52 clipboard addon is
  deliberately not a dependency, now asserted rather than assumed.
- Sessions started outside TerminalAI now appear in an **Elsewhere on this machine** panel, read
  from Claude Code's per-PID session registry with `claude agents --json` as the reconciliation
  fallback. The rows are read-only and carry no controls, because the supervisor owns none of
  those processes. Identity is `(pid, procStart)` so a reused PID cannot inherit another
  session's row, and an unreadable registry reports "unknown" rather than an empty machine.
- The cost model can now express the rates transcripts actually carry: separate 5-minute and
  1-hour cache-write tiers, the regional-inference premium and the priority-speed premium. The
  previous four-field type could represent none of them, so the first spend figure the fleet
  reported would have under-stated by up to half on a cache-heavy turn.
- A commit-pinned LiteLLM price snapshot is vendored at
  `crates/terminalai-core/pricing/model-prices.json` and embedded in the binary — never fetched at
  runtime — with a hardcoded fallback and a version string the fleet header names in its tooltip.
  Dated and region-prefixed model aliases resolve to their base model.
- Transcript usage parsing no longer resolves nested `usage` objects alphabetically (the JSON map
  is a `BTreeMap`, so a sibling key could win over `message`), and the deduplicated request-id set
  is bounded instead of growing for the life of the daemon.
- `branch` is now real: read from the session directory's Git HEAD at launch and refreshed from
  hook events, rate limited to one lookup per session per 30 seconds so a per-tool-call hook does
  not become a process per tool call. A directory outside a repository, a detached HEAD, or a Git
  that does not answer within 1.5 seconds all render an em dash instead of a guess.
- `tool_progress` is now populated from the agent's own plan — Claude Code's `TodoWrite` and
  Codex's `update_plan` both carry a countable list. A tool call with no plan leaves the previous
  value alone, and a session that has never reported one renders an em dash rather than `0/0`.
- A session that has never reported a cost renders an em dash instead of `$0.00`, in the row and
  in the fleet header. `Number(null)` is `0`, so the header was reporting a computed-looking zero
  for a figure nothing writes yet.
- The compact fleet row now meets its documented 28px, down from 54px, putting the status glyph,
  name, repository, agent, tool progress, restart count, dwell and last output line on one line
  and moving branch, ports and the spelled-out status into Wide. Measured in WebView2's engine at
  the default 1440x900 window: 29 fully visible rows against 13 before.
- Blocks carrying the `hidden` attribute are hidden again. An author `display: grid`/`flex`
  declaration outranks the user-agent `[hidden]` rule, so the Wide metadata and inline reply
  blocks were rendering on every row regardless.
- A session marked reviewed now returns to unreviewed as soon as its diff changes. The mark
  records a fingerprint of the diff it was made against instead of a write-once boolean, so a
  session no longer stays acknowledged while the agent keeps editing files. Marking a session
  whose working tree cannot be read is refused rather than recorded.
- Session exit detection now blocks on the child's process handle instead of a per-session thread
  waking twenty times a second to ask whether it had finished. Environment setup and teardown
  hooks use one bounded wait rather than a 25 ms poll. Measured over 60 seconds with ten idle
  sessions: 218.8 ms of CPU per minute before, below one scheduler tick after. Polling remains
  only where no waitable handle exists.
- `terminalai-probe cpu-idle` measures that supervision cost on demand, with `--poll` to reproduce
  the old strategy for comparison on the same machine.
- `panic = "abort"` is gone from the release profile. One panic on any daemon worker thread used
  to terminate every supervised session at once, and the poisoned-lock recovery arms were dead
  code in the shipped binary while the test profile forced unwinding. A build-time guard in
  `terminalai-daemon` refuses to compile under abort. Release binaries grow about 0.5 MB.
- The declared MSRV is now 1.88, the real floor set by resolved dependencies. The workspace
  claimed 1.82, which cannot build the tree — the README documents the `cargo metadata` command
  that re-derives the floor so the next dependency bump is caught rather than discovered.
- The NSIS and MSI bundles now ship `terminalai-daemon.exe` and `terminalai-probe.exe` as Tauri
  sidecars, so an installed copy can start. Previously the app resolved a sibling daemon the
  bundle never placed next to it, and every installed copy exited before drawing a window.
- New release gate `scripts/verify-installer.ps1` installs the bundle into a scratch prefix,
  asserts every declared sidecar arrived, launches the installed binary on the isolated display
  with placement proof, waits for the daemon control pipe, then uninstalls.
- An operator-configured agent path is now positively identified before it is spawned: its file
  stem must match the agent and a cached, deadline-bounded `--version` probe must name it.
  Previously the downstream agent/binary guard compared two values that were equal by
  construction, so it could never fire on the configured-path route.

## [0.1.0] — 2026-08-02

First working core. No GUI yet — everything below is exercised through `terminalai-probe`.

### Added

- `terminalai-core::agent` — resolves the native `claude.exe` and `codex.exe` behind the npm
  shims, via the global npm prefix or `PATH`, rejecting `.cmd`/`.ps1` wrappers that
  `CreateProcess` cannot execute.
- `terminalai-core::launch` — maps launcher choices (agent, model, effort, permission mode,
  sandbox, profile, extra dirs, resume/fork, spend cap, web search, initial prompt) to the exact
  argument vector for each CLI. Verified against Claude Code 2.1.170 and codex-cli 0.146.0.
  Options the chosen agent cannot express are refused rather than silently dropped.
- `terminalai-core::pty` — ConPTY session supervision: spawn, stream output to a sink, write,
  resize, poll for exit, kill.
- `terminalai-core::session` — the fleet-row model: status lattice ordered so anything awaiting
  the user sorts to the top, status dwell time, spinner-noise-tolerant last-line extraction.
- `terminalai-probe` — headless harness with `resolve`, `preview`, `spawn` and `exec`
  subcommands. `exec` runs any program on a pseudo-console, which separates pty faults from
  agent faults when a launch misbehaves.
- 16 unit tests covering flag mapping, refusal cases and fleet ordering.

### Fixed

- Headless ConPTY sessions produced no output and never exited. `portable-pty` always requests
  `PSEUDOCONSOLE_INHERIT_CURSOR`, so conhost emits a `ESC[6n` cursor-position query at startup
  and stalls the console until something answers. The reader thread now answers it with
  `ESC[1;1R`.
- Child exit was detected by waiting for the reader to hit EOF. On Windows the ConPTY master
  stays readable after the child is gone — conhost, not the child, owns the far end — so that
  wait never returns. Exit is now detected with `try_wait`.

[0.1.0]: https://github.com/SysAdminDoc/TerminalAI/releases/tag/v0.1.0
