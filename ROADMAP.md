# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; completed items are removed.

## v0.3.0 — knowing what the fleet is doing

- [ ] Transcript tailing — `~/.claude/projects/<slug>/*.jsonl` and Codex session rollouts, for
      last message, native session id, token and cost accounting
      — *2026-08-02 research: slug = cwd with `\`, `:`, `.` → `-`; filename stem IS the session UUID. Use
      `ai-title` for the row label and `last-prompt` for context. Codex rollouts live at
      `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO>-<uuid>.jsonl`. Cost is NOT in the JSONL — derive it, and
      dedupe on `requestId` first (see R-10).*
      — *2026-08-02 research: `TranscriptAccumulator` is exported at `lib.rs:65` and used nowhere, so
      `Session.cost_usd` is never assigned and `AdmissionSnapshot.aggregate_cost_usd` is always 0 — the fleet
      header reports a computed-looking zero. Before wiring it, fix three defects: `find_usage` recurses a
      `serde_json::Map` (a `BTreeMap` without `preserve_order`) so nested `usage` objects resolve
      alphabetically rather than document-first; records lacking `requestId` are summed unconditionally, which
      double-counts Codex's cumulative per-turn usage; and `seen_request_ids` grows unbounded. No pricing data
      ships. agent-deck v1.11.0 shipped this on 2026-08-01, so it is now parity work, not a differentiator.*
- [ ] Status inference fallback for sessions started outside TerminalAI
      — *2026-08-02 research: when no hook is installed for an agent, render a visible degraded state. Never
      report idle from absence of signal — that is the exact failure mode in ccmanager#227 and cmux#1027.*
- [ ] Pin up to three sessions to keep live grids; split view
- [ ] Windows toast on `NeedsYou`, with click-to-focus
      — *2026-08-02 research: an unpackaged Win32 app cannot raise a toast without a Start Menu shortcut carrying
      `System.AppUserModelID`. Use `tauri-winrt-notification`; click-to-activate additionally needs a COM activator
      registered under `HKCU\Software\Classes\CLSID\{...}\LocalServer32`, which that crate does not do. Pair with
      R-16 so toasts self-retract.*
      — *2026-08-02 research: prefer `tauri-winrt-notification` 0.8.1 over `tauri-plugin-notification` 2.3.3 —
      the plugin is documented to show the PowerShell name and icon in development and to work only for installed
      apps, and neither provides the COM activator. Fix R-48 first or toasts will outlive their sessions.*

## v0.4.0 — many sessions, one operator

- [ ] Scrollback to disk with a bounded in-memory ring per session
      — *2026-08-02 research: bound in BYTES, not lines (tmux#4859 — cost scales with width, so a line limit means
      3× the memory in a wide pane). Two-tier per Warp's block model: mutable grid for the live region, packed
      immutable bytes for scrollback.*
- [ ] Git worktree per session, created and cleaned up from the launcher
      — *2026-08-02 research: git isolation alone is insufficient; see R-22 for the non-git half (ports, services,
      databases), which HN 46424131 identifies as universally unserved.*
- [ ] Broadcast a prompt to a selected set of sessions
- [ ] Cost and token rollup across the fleet
      — *2026-08-02 research: dedupe on `requestId` before summing (R-10), and pair display with enforcement (R-17).
      No OSS competitor instruments cost at all — this is the least contested feature in the survey.*
- [ ] Filter and group the list by folder, agent, status

## v0.5.0 — beyond the terminal

- [ ] ACP transport as an alternative to the pty, for a compact native chat view
      (`@zed-industries/claude-code-acp`, `agentclientprotocol/codex-acp`)
      — *2026-08-02 research: downgraded to conditional. ACP v1 is stable but neither target CLI speaks it natively;
      only third-party adapters exist. Do this only when adding a third agent family.*
- [ ] Session templates per repo, read from the repo itself

## v0.6.0 — the project factory (operator request, 2026-08-03)

- [ ] Ship default presets — the launcher's preset store starts empty; seed it with useful
      out-of-the-box presets (e.g. Claude/Codex × plan-first/full-auto × common effort levels) that
      appear on first run, are clearly marked built-in, and can be hidden or cloned but not edited
      in place
- [ ] Usability and comprehension pass — a new operator should understand the fleet screen without
      reading the README. Cover: first-run guidance beyond the empty-state card, plain-language
      labels for status/dwell/attention states, tooltips on every control, and a short in-app
      explainer of the row → focused-terminal model. Acceptance: someone unfamiliar with the tool
      can launch a session, read its state, and answer an attention request unaided
- [ ] Master project folder — register a root (e.g. `~/repos`) once; every child git repo becomes a
      known project, kept current as repos appear and disappear, and usable as a launch target
      without browsing for a folder each time (extends the v0.4.0 folder filter/grouping)
- [ ] Roadmap scanner — for every known project, detect and parse `ROADMAP.md`, surfacing
      open-item counts and staleness per project so "which projects still have work queued" is a
      glance, not a hunt
- [ ] Per-session prompt queue — queue one or more prompts against a session while it is busy;
      when the current run completes, the next queued prompt is sent automatically. Queue is
      visible and reorderable per row, a queued prompt can be edited or withdrawn before it fires,
      and completion detection uses the same hook/status signal as the fleet's Idle state — never a
      timer. If the run ends in an attention state (approval or question), the queue pauses rather
      than answering blind
- [ ] Automated roadmap work queue — stored prompt library plus a queue that runs a chosen prompt
      against all (or selected) projects whose roadmaps still have open items, one session per
      project, honoring the fleet's admission/memory budget. First two stored prompts: "research
      new roadmap items" and "drain the roadmap". Queue survives restarts, reports per-project
      outcomes, and never launches into a repo with uncommitted changes without flagging it
      (builds on v0.4.0 broadcast; distinct from it — broadcast targets running sessions, the
      queue creates them)

## Research-Driven Additions

From `RESEARCH.md`. IDs R-01…R-63; the next researcher continues from R-64.

### P0

### P1

### P2

- [ ] R-57 · P2 — Correct terminal sizing and the focus-switch race
  Why: the terminal is hard-coded to 120×40 regardless of pane size, and output arriving during a focus switch is written to the wrong session.
  Evidence: `web/src/main.js:721-724` instantiates and registers `FitAddon` but never calls `.fit()`, and there is no window `resize` listener. `main.js:462-471` assigns `state.focused` after the awaited `reattach`; `hydrateTerminal` (`main.js:480-492`) resets then writes across another await.
  Touches: `web/src/main.js`
  Acceptance: the terminal fits its pane, with resize debounced into an explicit PTY resize request and never fired on splitter drag (agent TUIs hard-wrap and do not reflow, so a drag-driven resize corrupts the output being parsed for status); focus switches carry a generation token so late output for the previous session is discarded; replay uses DEC 2026 synchronized output and `onWriteParsed` (both new in `@xterm/xterm` 6.0.0, already pinned) to avoid tearing.
  Complexity: M

- [ ] R-58 · P2 — Harden the IPC message boundary
  Why: either end can be OOM-ed by a peer that sends bytes without a newline, and a malformed frame tears down the connection with no reply.
  Evidence: `terminalai-daemon/src/lib.rs:336,770` grow a `String` with no cap; `Request::Write { data: String }` (`lib.rs:106`) is unbounded; malformed JSON drops the connection at `lib.rs:339-340`; a transient `thread::Builder::spawn` failure inside the accept loop terminates `serve()` entirely (`lib.rs:281`), abandoning every live session.
  Touches: `terminalai-daemon/src/lib.rs`
  Acceptance: a maximum frame size is enforced on read with a typed error response; oversized `Write` payloads are rejected, not truncated; malformed JSON returns `Response::Error` and keeps the connection; a spawn failure logs and continues accepting.
  Complexity: S

- [ ] R-59 · P2 — Daemon lifecycle: shutdown, skew diagnosis, no duplicate spawn
  Why: nothing ever stops the daemon, and on protocol skew the app spawns a second one that cannot bind, then reports a generic timeout while the old daemon keeps running with live agents and no UI.
  Evidence: `serve()` (`lib.rs:270-285`) loops until killed — no console-control handler, no idle or last-client shutdown; the client's typed `VersionMismatch` arm (`lib.rs:690`) is dead code because the daemon answers `Response::Error` (`lib.rs:367-380`); `crates/terminalai-app/src/main.rs:261-300` treats any connect failure as "no daemon running"; `PIPE_NAME` embeds `v2` (`lib.rs:38`), so a future v3 orphans the v2 daemon permanently. `interprocess` already sets `FILE_FLAG_FIRST_PIPE_INSTANCE`.
  Touches: `terminalai-daemon/src/lib.rs`, `main.rs`, `crates/terminalai-app/src/main.rs`
  Acceptance: version mismatch surfaces a distinct actionable message naming the running daemon and how to stop it, and does not trigger a spawn; the daemon exposes a graceful shutdown request and a console-control handler; a single-instance guard prevents duplicate spawns.
  Complexity: M

- [ ] R-60 · P2 — Describe the trust boundary honestly and tighten the DACL
  Why: the peer check is self-consistency rather than authorization, and the DACL broadens under elevation — while the control pipe can run arbitrary shell commands.
  Evidence: `terminalai-daemon/src/lib.rs:322,381` compares `GetNamedPipeClientProcessId` against a client-declared `client_pid`; `lib.rs:821` grants `GA` to `OW`, which resolves to `Administrators` when an elevated process's token default owner is that group; `crates/terminalai-core/src/environment.rs:220-233` runs setup/teardown via `cmd.exe /c` from any pipe client's `LaunchSpec`.
  Touches: `terminalai-daemon/src/lib.rs`, `environment.rs`, `CLAUDE.md`, `README.md`
  Acceptance: the DACL names the interactive user's SID explicitly rather than `OW`; `IpcError::PeerMismatch` and the module docs state that the DACL is the boundary and the PID is diagnostic; shell hooks are opt-in per session and documented as local code execution.
  Complexity: S

- [ ] R-61 · P2 — Close the CSP and clipboard gaps
  Why: `base-uri` and `form-action` do not inherit from `default-src`, and agent output reaching the clipboard is a threat rather than a feature.
  Evidence: `crates/terminalai-app/tauri.conf.json:15` sets only `default-src`, `connect-src`, `img-src`, `style-src 'unsafe-inline'` and `script-src`; `@xterm/xterm` 6.0.0 added OSC 52 clipboard support; `main.js:362` is the one attribute interpolation bypassing `escapeHtml`.
  Touches: `tauri.conf.json`, `web/src/main.js`
  Acceptance: CSP adds `base-uri 'none'` and `form-action 'none'`; OSC 52 writes are disabled or require explicit per-session opt-in; the unescaped interpolation goes through `escapeHtml`.
  Complexity: S

- [ ] R-63 · P2 — Structured logging with bounded retention
  Why: the diagnostics timeline explains one session, but nothing records what the daemon did across sessions, and the crash log grows without limit.
  Evidence: `crates/terminalai-daemon/src/persistence.rs:48-51` appends to `crash.log` with no rotation; there is no `tracing` subscriber anywhere in the workspace, so a status misattribution — the dominant bug class in this field — leaves no trail beyond the in-memory timeline.
  Touches: `terminalai-daemon`, `terminalai-core`, GUI log panel
  Acceptance: `tracing` with one span per session carrying `session_id`/`agent`/`cwd`; `tracing-appender` with `Daily` rotation and `max_log_files` (there is no size-based rotation — tokio-rs/tracing#1940) writing under `%LOCALAPPDATA%\TerminalAI\logs\`; the `WorkerGuard` is held in `main` so the tail including panics is not lost; an in-app panel reads from a bounded `VecDeque` and is pushed to the WebView in batches on a ≥100 ms timer, never per event; `std::panic::set_hook` is installed before any thread spawns and covers PTY reader threads.
  Complexity: M

- [ ] R-62 · P2 — Finish the contrast and forced-colors work R-27 did not cover
  Why: R-27 was closed after the keyboard and labelling changes landed, but the measured contrast failures and high-contrast support were never addressed, so the acceptance criterion "contrast meets WCAG AA" is still unmet.
  Evidence: measured 2026-08-02 on Catppuccin Mocha — `overlay1` on `base` = 4.44 (fails AA by 0.06); `overlay0` (#6c7086) on `base` ≈3.8 and is still used for 9–10px text in `.row-folder`, `.eyebrow`, `.terminal-statusbar`, `.terminal-path`, `.empty-state p`, `.diagnostics-heading p` (`web/src/styles.css:39,57,61,63,71`); `--surface2` is used as text in `.terminal-grid` and `.terminal-placeholder small` (≈2.8:1); accents on `surface1` = 3.94–4.49, so selected rows must use `surface0`. No `@media (forced-colors: active)` or `prefers-reduced-motion` block exists anywhere in the stylesheet; `-ms-high-contrast` is dead as of Edge 138 and WebView2 is Chromium, so colour-only status vanishes in High Contrast.
  Touches: `web/src/styles.css`, `web/src/main.js`
  Acceptance: no text renders below 4.5:1; `overlay0`/`overlay1` are decorative only; selected rows use `surface0`; a `forced-colors` block maps to system keywords with `forced-color-adjust: none` limited to the xterm surface; `prefers-reduced-motion` disables the pulse and glow animations; xterm `screenReaderMode` is an opt-in setting (it breaks right-click copy/paste, xterm.js#1931).
  Complexity: M

- [ ] R-29 · P2 — Versioned session store with a migration path
  Why: session state will outlive its schema, and a daemon that survives GUI upgrades will meet older files.
  Evidence: Zellij versions its session-info directory by release; RESEARCH.md "Architecture".
  Touches: `store.rs`, `terminalai-daemon/src/persistence.rs`, test fixtures
  Acceptance: the file carries a magic string plus an integer version (SQLite `application_id`/`user_version` shape); older versions are copied to `sessions.v<old>.bak` before migrating; unknown fields survive a round-trip via `#[serde(flatten)]`; one fixture per historical version asserts migration to the current shape.
  Complexity: S
  — *2026-08-02 research: depends on R-34 (atomic writes) and R-36 (quarantine). As written this item assumes the failure paths work; today a version mismatch is indistinguishable from corruption and both brick the daemon, so do those first. Chrome's `Last Version` is the precedent for refusing unknown-newer loudly rather than best-effort parsing.*

### P3

- [ ] R-31 · P3 — `AgentDomain` trait for non-local sessions
  Why: remote execution is what every commercial competitor charges for; introducing the seam now keeps it from being a rewrite later.
  Evidence: WezTerm's `Domain` trait abstracts local/WSL/SSH/mux behind spawn/split/attach/detach; RESEARCH.md paywall analysis item 1.
  Touches: `crates/terminalai-core`
  Acceptance: local ConPTY is one implementation of the trait; no call site assumes a local process handle.
  Complexity: M

- [ ] R-32 · P3 — Internationalization scaffolding
  Why: retrofitting string extraction after the UI exists is far more expensive than starting with it, even though the initial audience is English-only.
  Evidence: no competitor in the survey ships localization; wmux is actively translating post-hoc.
  Touches: GUI string layer, daemon message types
  Acceptance: strings live in a single Fluent catalog loaded by Rust and formatted in JS — one catalog for both sides, avoiding the guaranteed drift of a JS-only solution; the daemon emits `reason` enums with arguments rather than English prose (which also feeds diagnostics); `Intl.RelativeTimeFormat`/`PluralRules` handle dwell times and counts; the 28px row survives ~2× string growth by keeping status as glyph-plus-number with the word in the tooltip.
  Complexity: M
  — *2026-08-02 research: German/Finnish average +20–35% and up to 2× on short strings ("Queued" → "In der Warteschlange" is 3.2×), so fixed-px columns must become `ch`/`minmax()` first. Do this after R-52/R-53, which change the row markup anyway.*

## Research-Driven Additions — external survey, 2026-08-02

From the same-day external research pass in `RESEARCH.md` (competitors, community signal, agent-platform APIs,
dependency and CVE review). IDs R-64…R-87 do not overlap R-34…R-63, which came from the internal code audit; the next
researcher continues from R-88.

### P0

- [ ] R-64 · P0 — Ship the daemon and probe inside the installer
  Why: the installed application cannot start — the app requires a sibling `terminalai-daemon.exe` that the bundle never places next to it, so every user who installs rather than builds gets a process that exits before drawing a window.
  Evidence: `crates/terminalai-app/tauri.conf.json` declares `bundle.targets` and `webviewInstallMode` but no `externalBin` or `resources`; `crates/terminalai-app/src/main.rs:266-273` resolves `terminalai-daemon.exe` beside `current_exe()` and errors if absent. Verified 2026-08-02: `TerminalAI_0.1.0_x64-setup.exe` is 1.1 MB against a 3.4 MB `terminalai.exe` plus a 924 KB daemon.
  Touches: `crates/terminalai-app/tauri.conf.json`, release checklist, `README.md`
  Acceptance: the NSIS and MSI bundles contain `terminalai-daemon.exe` (and `terminalai-probe.exe` if it is a supported entry point); a release-gate step installs into a scratch prefix, launches the installed binary, asserts the fleet window appears and the daemon starts, then uninstalls; the check fails loudly if any declared sidecar is missing from the bundle.
  Complexity: S

- [ ] R-65 · P0 — Reveal the focused terminal
  Why: the xterm element is appended after a placeholder that nothing ever removes, and both are full-height inside an `overflow: hidden` host — so the renderer this project is built around is laid out entirely outside the visible box.
  Evidence: `web/index.html:55` renders `.terminal-placeholder`; `web/src/main.js:723` calls `terminal.open($("terminal-host"))`, which appends; grep for `terminal-placeholder` in `web/src/main.js` returns zero matches. `web/src/styles.css:61` sets `.terminal-host { overflow: hidden }` with `.terminal-placeholder { height: 100% }` and `.terminal-host .xterm { height: 100% }`.
  Touches: `web/index.html`, `web/src/main.js`, `web/src/styles.css`
  Acceptance: the placeholder is removed or hidden the first time a session is focused and restored when focus clears; the xterm element occupies the host box; a UI test (R-56) focuses a session and asserts the terminal renders with non-zero height.
  Complexity: S

- [ ] R-66 · P0 — Closing the launcher must not launch a session
  Why: the dialog's close control and the Enter key in any launcher field both submit the form, and the submit handler spawns an agent — an unrecoverable action that costs tokens and may write to a repository the user never chose.
  Evidence: `web/index.html:66` — `<button class="dialog-close" value="cancel" aria-label="Close launcher">` has no `type`, so inside `<form id="launcher-form">` (`index.html:65`) it defaults to `submit`; `web/src/main.js:820-823` calls `event.preventDefault()` then `launchCurrentSpec()`, defeating `method="dialog"`. The sibling controls at `index.html:88` correctly carry `type="button"`.
  Touches: `web/index.html`, `web/src/main.js`
  Acceptance: every non-launch control in the dialog carries `type="button"`; Enter in a text field does not launch (explicit submit only, or Ctrl+Enter); a test presses Escape, clicks the close control, and presses Enter in the preset-name field, asserting zero launches in all three cases.
  Complexity: S

- [ ] R-67 · P0 — Verify the agent binary instead of asserting it
  Why: the guard meant to stop a launcher configuration starting the wrong CLI compares two values that are equal by construction, so it can never fire on the configured-path route — and a check that always passes reads exactly like one that ran.
  Evidence: `crates/terminalai-core/src/agent.rs:84-97` — `resolve()` returns any configured path where `is_file()` holds, stamping the *requested* `agent` onto it with `Origin::Configured` and never inspecting the file. `crates/terminalai-core/src/registry.rs:363-368` then rejects only when `spec.agent != binary.agent`. The CHANGELOG claims "strict agent/binary matching before a process is spawned".
  Touches: `crates/terminalai-core/src/agent.rs`, `registry.rs`, `crates/terminalai-core/tests/`
  Acceptance: a configured path is accepted only after positive identification — file stem matches the agent's expected executable name, and a cached `--version` probe matches that agent's expected banner; mismatches return a typed error naming both sides; a test points the Claude configured path at `codex.exe` and at an unrelated executable and asserts both are refused.
  Complexity: S

### P1

- [ ] R-68 · P1 — Give the daemon unwinding, or stop it panicking
  Why: `panic = "abort"` in the one process whose entire job is survival means a single panic on any worker thread kills every supervised session at once — the blast radius is the whole fleet, and no dependency upgrade can fix it.
  Evidence: `Cargo.toml:37`. The daemon runs a thread per connection (`crates/terminalai-daemon/src/lib.rs:274`), per writer (`lib.rs:325`), per PTY reader (`crates/terminalai-core/src/pty.rs:108,331`), per restart timer and per process monitor (`registry.rs:1081,1146`), with roughly 40 `.expect("registry poisoned")` sites. Secondary effect: every `PoisonError` arm is dead code in release while the test profile forces unwind, so R-46's tests will pass without proving anything about the shipped binary.
  Touches: `Cargo.toml`, `crates/terminalai-daemon/`, release checklist
  Acceptance: the daemon binary is built under a profile without `panic = "abort"` (the GUI may keep it) and R-46's recovery path is exercised under that profile; or, if abort is kept deliberately, every worker-thread body is audited free of `unwrap`/`expect`/indexing and the decision plus its blast radius is recorded in `CLAUDE.md`.
  Complexity: S

- [ ] R-69 · P1 — Correct the MSRV declaration and the README badge
  Why: the workspace claims a minimum Rust it cannot build on, so the first person to trust the badge — or to add an MSRV CI job — gets a failure that looks like a broken toolchain.
  Evidence: `Cargo.toml:13` sets `rust-version = "1.82"` and `README.md:6` badges 1.82+. Verified 2026-08-02 via `cargo metadata --filter-platform x86_64-pc-windows-msvc`: 28 resolved packages declare MSRV 1.85 or higher, seven declare 1.88.0 (`darling`, `plist`, `time`, `serde_with` and friends, via the Tauri bundler chain), and 12 are edition 2024.
  Touches: `Cargo.toml`, `README.md`, `CLAUDE.md`
  Acceptance: `rust-version` states the true floor (1.88 today) and the badge matches; a documented command re-derives the floor from `cargo metadata` so the next dependency bump is caught rather than discovered.
  Complexity: S

- [ ] R-70 · P1 — Populate or retract `branch`, `tool_progress` and `cost_usd`
  Why: three columns the README sells as working are hard-wired to nothing, so the fleet row shows em dashes and the header shows a computed-looking `$0.00` — the product currently advertises fields no code writes.
  Evidence: `crates/terminalai-core/src/session.rs:201,214,218` set all three to `None` at construction; verified by grep, no writer exists anywhere in `crates/` for `branch` or `tool_progress`, and `cost_usd` is only ever read (`registry.rs:265`). `README.md:80-81` documents all three as visible in the compact and Wide rows.
  Touches: `crates/terminalai-core/src/session.rs`, `registry.rs`, `README.md`, `web/src/main.js`
  Acceptance: `branch` is resolved per session from the cwd's git HEAD and refreshed on hook events; `tool_progress` is populated from the hook and app-server signals that carry a countable plan, rendering an em dash only when the agent genuinely exposes none; any field not wired in this release is removed from `README.md` in the same change.
  Complexity: M

- [ ] R-71 · P1 — Make the cost model arithmetically correct before it is shown
  Why: the pricing type cannot express the rates the transcripts actually carry, so the first number the fleet reports will be confidently wrong — and a spend figure that under-reports is worse than none, because admission control is meant to act on it.
  Evidence: `crates/terminalai-core/src/transcript.rs:12-27` — `TokenRates` has four fields and cannot express the 5-minute versus 1-hour cache-write split (1.25x / 2x), the `inference_geo: "us"` 1.1x multiplier, or the `speed: "fast"` Opus-5 rate; all three appear in `message.usage` in transcripts on this machine, alongside `service_tier`. No pricing data ships at all. Neither vendor publishes a machine-readable price table; LiteLLM's `model_prices_and_context_window.json` (MIT) is the only maintained one — verified live, 2,986 entries, already carrying `claude-opus-5` and `gpt-5.6-luna`.
  Touches: `crates/terminalai-core/src/transcript.rs`, build script, fleet header
  Acceptance: `TokenRates` models both cache-write tiers and the geo and speed multipliers; a price table is vendored at build time from a commit-pinned LiteLLM snapshot with a hardcoded fallback, never fetched at runtime; the UI states "prices as of <date>" and the pinned table version; a test prices a real transcript record carrying `inference_geo` and `speed` and asserts the multiplier is applied.
  Complexity: M

- [ ] R-72 · P1 — Adopt sessions TerminalAI did not spawn
  Why: Claude Code ships an official, headless enumeration of every running session on the machine, so the supervisor can show and reason about agents started from a terminal, an IDE or another tool instead of pretending they do not exist.
  Evidence: verified 2026-08-02 — `claude agents --json` returned 11 live sessions with `pid`, `cwd`, `sessionId`, `startedAt`, `kind` and `name`, backed by a per-PID registry at `~/.claude/sessions/<pid>.json` that additionally carries `procStart` (making identity PID-reuse-safe), `version` and `entrypoint`. `claude attach <id>` and `claude logs <id>` exist but are absent from `--help`. Codex's equivalent is `thread/list` with `sourceKinds` and `archived` filters.
  Touches: `crates/terminalai-core/src/agent.rs`, `registry.rs`, fleet list
  Acceptance: externally started sessions appear as read-only rows identified by `(pid, procStart)`, visibly distinguished from supervised rows and never offered actions the supervisor cannot perform; the registry file is watched directly with `claude agents --json` as the reconciliation fallback; an unparseable or missing registry degrades to "unknown", never to "idle".
  Complexity: M
  — *Supersedes the premise of the v0.3.0 item "Status inference fallback for sessions started outside TerminalAI", which assumed no signal exists. Keep that item's rule — never report idle from absence of signal — and source it from here.*

- [ ] R-73 · P1 — Wait on the process handle instead of polling it
  Why: the project's stated principle is "push, not poll", yet every live session runs a dedicated thread waking twenty times a second purely to ask whether a process has exited — 600 wakeups per second at the documented thirty-session target, on battery.
  Evidence: `crates/terminalai-core/src/registry.rs:1148-1162` — a per-session monitor thread loops `pty.try_wait()` with `thread::sleep(Duration::from_millis(50))`, or 250 ms after an error. Windows exposes a waitable child handle; `environment.rs:202` polls at 25 ms for the same reason.
  Touches: `crates/terminalai-core/src/registry.rs`, `pty.rs`, `environment.rs`
  Acceptance: exit detection blocks on the child handle, or on a single wait-many thread servicing all sessions, with no periodic wakeup; CPU at idle with ten tracked live sessions is measured before and after and recorded; the settle-window behaviour the ConPTY EOF trap requires is preserved and still covered by `tests/pty_echo.rs`.
  Complexity: S

- [ ] R-74 · P1 — Reset `reviewed` when the diff changes
  Why: the review surface's only piece of operator state is write-once, so a session marked reviewed stays marked while the agent keeps changing files — the exact "already handled" false negative a review queue exists to prevent.
  Evidence: `crates/terminalai-core/src/registry.rs:591-593` sets `session.reviewed = true`; grep finds no other writer, and `crates/terminalai-core/src/review.rs:62` copies it into every `ReviewItem`.
  Touches: `crates/terminalai-core/src/registry.rs`, `review.rs`, `session.rs`
  Acceptance: the reviewed mark records the diff state it was made against — a hash of the numstat, or HEAD plus a dirty-file digest — and clears automatically when that changes; a test marks a session reviewed, writes a file, and asserts the row returns to unreviewed.
  Complexity: S

- [ ] R-75 · P1 — Meet the documented row density, or change the documents
  Why: the 28px row is the project's central design claim and the reason it says thirty sessions fit on one screen; the shipped row is nearly twice that, so the headline number is unreachable at the default window size.
  Evidence: `web/src/styles.css:55` — `.fleet-row { min-height: 54px }`, inside `.fleet-list { height: calc(100% - 111px) }` within `calc(100vh - 72px)`. At the configured 1440x900 default (`tauri.conf.json`), that is roughly 13 visible rows. `README.md:22` and `CLAUDE.md:41` both specify about 28px.
  Touches: `web/src/styles.css`, `web/src/main.js`, `README.md`, `CLAUDE.md`
  Acceptance: either the compact row reaches about 28px — status glyph, name, repository, dwell and last line on one line, with the second line moving into Wide — or both documents are corrected to the real figure and the "thirty sessions on one screen" claim is restated against a measured window size; a test asserts the rendered compact row height against the documented value so the two cannot drift again.
  Complexity: M

### P2

- [ ] R-76 · P2 — Bring the app's own commands under the Tauri ACL
  Why: the app grants nine core permission sets in order to use two API functions, and its commands are covered only by a runtime patch rather than by declared policy — so a future regression, or any remote-origin capability, has nothing to fail closed against.
  Evidence: `crates/terminalai-app/capabilities/default.json` grants `core:default`, which expands to `core:{path,event,window,webview,app,image,resources,menu,tray}:default`; `web/src/main.js:1-2` imports only `invoke` and `listen`. `crates/terminalai-app/build.rs` is a bare `tauri_build::build()`, and Tauri 2.11.1's advisory fix notes that without an `AppManifest`, custom commands previously bypassed ACL entirely. `tauri-build` 2.6.3 exposes `AppManifest::commands(&[...])`.
  Touches: `crates/terminalai-app/build.rs`, `crates/terminalai-app/capabilities/default.json`
  Acceptance: `build.rs` declares an `AppManifest` listing every `#[tauri::command]`, turning them into explicit `allow-*` permissions; the capability drops to the sets actually used, sets `"local": true`, omits `remote`, and adds `"platforms": ["windows"]`; a build assertion or test fails if a new command is added without a corresponding grant.
  Complexity: S

- [ ] R-77 · P2 — Widen hook coverage and ingest over HTTP
  Why: the supervisor observes a small fraction of the lifecycle both agents emit, and it ingests through `command` hooks, which on Windows cost a process spawn per event — on the platform whose documented weakness is precisely per-spawn console cost.
  Evidence: Claude Code documents 31 hook events across five handler types, including `type: "http"` with `$VAR`-interpolated headers gated by the `allowedHttpHookUrls` and `httpHookAllowedEnvVars` settings; Codex's `HookEventName` enum carries 11 (`preToolUse, permissionRequest, postToolUse, preCompact, postCompact, sessionStart, sessionEnd, userPromptSubmit, subagentStart, subagentStop, stop`) where the published docs list seven. `crates/terminalai-core/src/hook_config.rs` installs a narrower set.
  Touches: `crates/terminalai-core/src/hook_config.rs`, `hooks.rs`, `crates/terminalai-daemon/`
  Acceptance: the installed hook set covers every event that changes a row — session start and end, prompt submit, tool use and failure, permission request and denial, subagent start and stop, compaction, stop; ingest uses a loopback `type: "http"` endpoint carrying a startup-generated bearer token in `headers`, with a literal `Host` allowlist and `Origin` rejection, falling back to `command` hooks where HTTP is unavailable; unknown event names are retained and surfaced rather than dropped.
  Complexity: M

- [ ] R-78 · P2 — Feature-detect models and reasoning efforts
  Why: the launcher's model and effort lists are compile-time constants against products that change monthly, so the GUI will refuse valid combinations and offer dead ones — and it already disagrees with the published documentation in both directions.
  Evidence: `crates/terminalai-core/src/agent.rs` `suggested_models` and `crates/terminalai-core/src/launch.rs` `supported_efforts` are static. Verified 2026-08-02: `codex debug models` reports `max` as supported only on the gpt-5.6 family (sol, terra, luna) and absent from gpt-5.5/5.4/5.3; the app-server schema types `ReasoningEffort` as a non-empty string with per-model `supportedReasoningEfforts`, while the published config reference omits `max` entirely. Claude exposes `system/init.capabilities[]` for the same purpose, and this machine runs two Claude Code versions simultaneously (2.1.170 on PATH, 2.1.220 in-process), so version comparison is not a usable proxy.
  Touches: `crates/terminalai-core/src/agent.rs`, `launch.rs`, launcher UI
  Acceptance: model and effort options are populated from runtime probes (`model/list` with `supportedReasoningEfforts`, `system/init.capabilities[]`, `codex features list`) cached per resolved binary and invalidated on version change; an unknown but user-supplied value is passed through with a warning rather than refused; no capability decision reads a hardcoded list or compares version strings.
  Complexity: M

- [ ] R-79 · P2 — Make Windows process hygiene the measured differentiator
  Why: this is the one axis no competitor can copy cheaply — they are all Electron, Node or tmux — and both agent CLIs have open defects that worsen with session count, so the claim is worth proving rather than asserting.
  Evidence: anthropics/claude-code#66540 (open) — subprocesses spawn without `windowsHide`/`CREATE_NO_WINDOW`, so N sessions times M MCP servers flash M+1 console windows per tool call, with reported sustained keystroke loss at 12 concurrent sessions; #74107 (open) requests persistent terminal sessions for the same reason; #67220 (open) requests a native Windows toast channel; openai/codex discussion #29949 documents process-enumeration storms on high-spec Windows machines. A ConPTY-hosted child inherits the pseudoconsole rather than allocating a new conhost. Windows also exposes `SetProcessInformation(ProcessPowerThrottling)` (EcoQoS) and `ProcessMemoryPriority`, and Tauri 2.11.5 exposes `set_progress_bar`, `set_overlay_icon` and `set_badge_count`.
  Touches: `crates/terminalai-core/src/pty.rs`, `registry.rs`, `crates/terminalai-app/src/main.rs`, `README.md`
  Acceptance: a repeatable measurement counts console-window creations and input latency for N supervised sessions versus N terminal-launched sessions, and the result is published in the README with its method; background sessions (neither focused nor pinned) get EcoQoS and lowered memory priority, restored on focus; the taskbar shows the waiting-session count via overlay or badge. Depends on R-40 for job objects.
  Complexity: M

- [ ] R-80 · P2 — Harden the ConPTY DLL search path
  Why: the pty layer asks Windows to load `conpty.dll` by bare name before falling back to the system copy, and no such file exists in System32 on this OS build — so the search reaches directories a non-administrator can write.
  Evidence: `portable-pty` 0.9.0 `src/win/psuedocon.rs:52-55` calls `ConPtyFuncs::open(Path::new("conpty.dll"))` first; verified 2026-08-02 that `C:\Windows\System32\conpty.dll` does not exist on 26100, and no `SetDefaultDllDirectories` or `SetDllDirectory` call exists anywhere in `crates/`. wezterm#7775 separately documents that the older bundled ConPTY pair crashes shells using the alternate screen buffer on exit, fixed by the matched 1.24 pair.
  Touches: `crates/terminalai-daemon/src/main.rs`, release checklist
  Acceptance: the daemon calls `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` before any pty is created; optionally the matched `conpty.dll` and `OpenConsole.exe` pair ships beside the daemon so the application directory wins deterministically, with its version pinned and recorded; a note in `CLAUDE.md` records why bare-name loading is unsafe here.
  Complexity: S

- [ ] R-81 · P2 — Per-session environment leases beyond ports
  Why: this is the most-repeated unsolved complaint in the entire community corpus — worktrees isolate files and nothing else, so parallel agents collide on ports, databases, docker projects and untracked config, and several people abandoned parallel agents specifically over it.
  Evidence: HN 46424131 ("none of them mention databases... ten different copies of my database"), 47870590 (quit after a sprint over test-data isolation and a shared migration), 47871667 ("can't easily copy secrets, ports conflict"), 48244818 (per-worktree docker compose prefix, hand-rolled), 47004368 ("two weeks just getting a second copy of the dev environment running"). Both Superset and Conductor explicitly punt to a user-written setup script. R-22 already ships deterministic port blocks and setup/teardown hooks; this extends that seam.
  Touches: `crates/terminalai-core/src/environment.rs`, launcher, session store
  Acceptance: a declarative lease per session covering ports (existing), copied untracked config by glob, a docker compose project prefix, and a database provisioned from a template or branch, all torn down when the session is archived; leases are declared per repository and versioned with it; a raw script escape hatch remains; teardown failures are surfaced, never swallowed. Ship depth on a small set of stacks rather than a generic hook API.
  Complexity: L

- [ ] R-82 · P2 — A land gate for finished sessions
  Why: every tool in the survey stops at "open a PR", so the operator still serialises and tests each landing by hand — and the community's second-ranked failure is semantic conflicts between agents that each looked correct alone.
  Evidence: HN 47870607 (two agents rename the same type differently; "neither worktree is wrong but the code is incoherent"), 49104747 (a hand-built local merge queue serialising commits and running the full suite per landing), 45110915 (merge queue with bisection to find the bad patch set). wmux is the strongest prior art: per-hunk selection across files combined into one all-or-nothing `git apply`, resolved against a fresh read of the worktree at adopt time and refused whole if the target moved, is dirty, or a hunk no longer applies.
  Touches: `crates/terminalai-core/src/review.rs`, new land module, review UI
  Acceptance: a session's changes can be landed from the review surface through a serialised queue that re-reads the target at land time, runs a configured verify command, and refuses the whole landing — never a partial one — if the target moved, the tree is dirty, the verify fails, or conflict markers are present; refusals name the specific condition; nothing is auto-resolved and no merge is mutated on the operator's behalf.
  Complexity: L

- [ ] R-83 · P2 — Rate limit and quota as a first-class row state
  Why: quota exhaustion is the failure operators actually hit — measured in hours, not days — and a rate-limited session currently renders as an ordinary busy or idle row, so the fleet looks healthy while doing nothing.
  Evidence: HN 47221592 ("Max plan limits in under an hour" with parallel agents), 47224276 ("weekly quota by day 3-4" on $200/mo), 47626833 ("20% of the weekly limit in about 2 hours"). Codex already pushes this: rollout `event_msg.token_count` carries `rate_limits` with `used_percent`, `window_minutes`, `resets_at`, `plan_type` and credit balance, and `account/rateLimits/read` returns the same over JSON-RPC. Claude's headless `system/api_retry` events carry `rate_limit` and `overloaded` error categories. Only amux and TUICommander model this at all.
  Touches: `crates/terminalai-core/src/session.rs`, `app_server.rs`, `registry.rs`, fleet row
  Acceptance: a rate-limited session shows a distinct status with its reset time, sorts with the attention states rather than with idle, and is excluded from admission's live count so a queued session can take the slot; the fleet header shows how many sessions are limited and when the earliest resets; the state is never inferred from silence.
  Complexity: M

- [ ] R-84 · P2 — Close the renderer capability gap
  Why: the app runs the slowest available renderer and re-fetches scrollback over IPC when the pinned dependencies already provide faster paths — and the two libraries either side of the terminal both implement synchronized output that neither is asked to use.
  Evidence: `web/package.json` pins `@xterm/xterm` 6.0.0 with only `@xterm/addon-fit`; 6.0 removed `addon-canvas`, so with no `@xterm/addon-webgl` the DOM renderer is in use. `@xterm/addon-serialize` 0.14.0 would replace the `invoke("scrollback")` round-trip at `web/src/main.js:484`. `vte` 0.15 implements DEC mode 2026 synchronized updates, OSC 8 hyperlinks and the Kitty keyboard protocol (`ansi.rs:45,48,967,701`); `crates/terminalai-core/src/grid.rs` exposes none of them. `allowProposedApi: false` (`main.js:698`) additionally blocks the unicode11 and ligature addons.
  Touches: `web/package.json`, `web/src/main.js`, `crates/terminalai-core/src/grid.rs`
  Acceptance: the WebGL addon is loaded with a documented DOM fallback when context creation fails; DEC 2026 synchronized output is honoured on both the vte and xterm sides so replay and live output do not tear; OSC 8 hyperlinks survive into the rendered pane; scrollback restore uses the serialize addon where it is cheaper than the IPC round-trip. Overlaps R-57 on sizing — do that first.
  Complexity: S

- [ ] R-85 · P2 — Dependency maintenance with a testability payoff
  Why: two pinned crates are behind in ways that bear directly on this project's hardest-to-test surface and its binary size, and one of them fixes the exact silent-miss mode agent resolution suffers from.
  Evidence: `which` is pinned at 7 (locking 7.0.3) against 8.0.5 — 8.0.0 adds a `Sys` trait allowing agent resolution to be unit-tested against an injected filesystem rather than the real PATH (which `CLAUDE.md` currently treats as probe-only territory), 8.0.4 emits a `NonFatalError` when Windows `PATHEXT` is unpopulated and no extension was given, and 8.0.5 stops using the current directory when the provided path is absolute. `toml_edit` is pinned at 0.20.2 (2023) against 0.25.x, and the lockfile currently carries three separate parser copies (0.19.15, 0.20.2, 0.25.13) in a binary built with `opt-level = "z"`. Tested 2026-08-02: 0.20.2 handles the 0.25.x regression inputs cleanly, so this is a size and testability change, not a security one.
  Touches: `Cargo.toml`, `crates/terminalai-core/src/agent.rs`, `hook_config.rs`
  Acceptance: `which` 8.x adopted, with agent resolution covered by tests against an injected filesystem including a missing-`PATHEXT` case; `toml_edit` unified on one version across the lockfile; release binary size recorded before and after. Note that `toml_edit` 0.25 requires Rust 1.85 — land R-69 first.
  Complexity: S

- [ ] R-86 · P2 — Cut the release the changelog already describes
  Why: a release worth of shipped work sits under "Unreleased" while every version string still reads 0.1.0, and three documents describe a product that no longer matches the code — the repository's own versioning and doc-sync rules are unmet.
  Evidence: `CHANGELOG.md` "Unreleased" lists the registry, Tauri shell, daemon, hook ingress, grids, restore ladder, notifications, admission control, app-server transport, probe control client, review surface and diagnostics, all against `[0.1.0] - 2026-08-02`. `Cargo.toml`, `crates/terminalai-app/tauri.conf.json` and `web/package.json` all read 0.1.0. `CLAUDE.md:30` says "25 tests" against a real 99; `README.md:53` says the Tauri shell is not built.
  Touches: `CHANGELOG.md`, `Cargo.toml`, `crates/terminalai-app/tauri.conf.json`, `web/package.json`, `README.md`, `CLAUDE.md`
  Acceptance: a version is cut with all strings matching and the changelog section dated; `CLAUDE.md` and `README.md` state the true test count, build status and verified CLI versions; the release gate runs the existing notes verification before publishing. Do this after R-64, so the released artifact is one that actually starts.
  Complexity: S

### P3

- [ ] R-87 · P3 — Expose the fleet as a read-mostly MCP server
  Why: no tool in the survey unifies both vendors' session lists behind one interface, so an agent cannot ask what its siblings are doing — and the read half of that is cheap once R-72 lands.
  Evidence: prior art is spawn-only or single-vendor — agent-dispatch delegates to `claude -p` in other directories, claude-code-mcp is one-shot and single-agent, pal-mcp-server does cross-vendor subagent spawning; Kandev and Paseo expose their own platforms over MCP but not a unified cross-vendor session list. MCP spec 2026-07-28 removed protocol-level sessions and `Mcp-Session-Id` from Streamable HTTP, making the transport stateless and pushing state back to the server. Tool poisoning — malicious instructions in tool metadata, re-executed on every invocation — is the dominant 2026 MCP attack class per the NSA/CISA advisory and Microsoft's June 2026 warning.
  Touches: new crate or daemon subcommand, `crates/terminalai-daemon/`
  Acceptance: a stdio MCP server exposes read tools (list sessions, read status, read a session's last output, read fleet cost) with no gating; any mutating tool (spawn, kill, send) is opt-in per session, requires an out-of-band token, and logs every invocation into the diagnostics timeline; the server refuses to expose transcript content by default. Depends on R-72.
  Complexity: M
