# Changelog

All notable changes to TerminalAI are recorded here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## Unreleased

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
- Reversible agent hook configuration now previews, installs, reports and removes only
  `--terminalai-managed` Claude JSON and Codex TOML entries; Claude handlers are asynchronous,
  Codex preserves unrelated `notify` commands, and the installed app hook path fails open when the
  daemon is unavailable.
- The fleet list now exposes distinct glyphs and visible labels for every status, keyboard row
  activation, screen-reader announcements, light/dark contrast tokens, forced-colors support and
  reduced-motion behavior.
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
