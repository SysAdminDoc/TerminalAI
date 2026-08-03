# Changelog

All notable changes to TerminalAI are recorded here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Added

- The Windows pipe ACL now grants the current interactive user's explicit SID
  (plus `SYSTEM`) instead of the elevation-sensitive owner-rights alias. PID
  mismatches are reported as diagnostics, and setup/teardown hooks are clearly
  documented as opt-in local shell execution.
- The daemon now has a graceful `terminalai-probe shutdown` control request, Windows console
  teardown handling, and a single-instance binding guard. Protocol skew reports the running daemon
  PID and a concrete stop command; the desktop shell refuses to spawn a second daemon in that case.
- The control endpoint now has a stable name with a legacy v2 fallback, so protocol negotiation can
  detect upgrades without stranding a daemon that still owns live sessions.

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
