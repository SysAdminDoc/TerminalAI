# TerminalAI

[![version](https://img.shields.io/badge/version-0.9.0-blue.svg)](CHANGELOG.md)
[![license](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](#requirements)
[![rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)

A control surface for running many Claude Code and Codex sessions at once.

Pick the agent, model, effort, permission mode and project folder from a GUI; TerminalAI
launches the real CLI on a pseudo-console and puts it in a dense fleet list. Twenty-nine tracked
sessions fit on one screen at the default 1440x900 window because only the ones you are actually
looking at get a terminal grid — the rest are a status row.

## Why another one

Every multiplexer in this space renders **panes**. Panes do not stack: four xterm panes fill a
1440p display. TerminalAI splits the problem in two.

| | Needs | Cost |
|---|---|---|
| **Attention data** — busy? blocked on approval? which model? how long? | ~28px | a ring buffer |
| **Interaction surface** — the actual TUI | ~600px | a terminal renderer |

You only ever type into one session at a time, so only one session pays for a renderer
(plus any you pin). Everything else is a row.

The compact row is measured, not aspirational: 28px exactly, carrying the status glyph, session
name, repository, agent, tool progress, restart count, dwell time and last output line on one
line. Measured 2026-08-03 in WebView2's engine at the default 1440x900 window, that is 29 fully
visible rows. **Wide** adds a second line with model, reasoning effort, branch, ports, the
spelled-out status and reported cost, at 50px.

Thirty is a tracked-session target, not a promise to keep thirty agent processes hot. Local
measurements on 2026-08-02 showed roughly 509 MB RSS for Claude Code and 322 MB for Codex per
live process; real work should stay within the machine's memory budget, typically two or three
parallel agents. The daemon can hibernate idle sessions while retaining their rows.

Status comes from agent hooks and transcript tailing — pushed, not polled. Polling a pty on a
timer, which is the common approach, both misses transitions between ticks and burns CPU per
session. Exit detection blocks on the child's own process handle for the same reason: measured on
2026-08-03 with `terminalai-probe cpu-idle --sessions 10 --seconds 60`, supervising ten idle
sessions cost 218.8 ms of CPU per minute when polling at 50 ms and less than one scheduler tick
when waiting on the handle.

## Status

**v0.9.0 — core and desktop shell built, installed, and verified end to end.**

Working today (`terminalai-probe`, headless):

- Resolves the real `claude.exe` / `codex.exe` behind the npm shims
- Builds the exact argument vector for any launcher combination, per agent
- Spawns and drives either agent on a live ConPTY
- Runs a Tauri 2.11.5/WebView2 shell with the Catppuccin fleet list, launcher, presets and one
  reused xterm renderer
- Preserves PTY and replay output as raw bytes end to end, delivering focused-session batches to
  xterm without corrupting multi-byte sequences split across reads
- Windows PTY sessions use kill-on-close job objects so stopping a session reaps its descendants;
  daemon shutdown runs active teardown hooks once
- Background ConPTY sessions use Windows EcoQoS and low memory priority while neither focused nor
  pinned; focus or pinning restores normal priority. Waiting-session counts appear as a numeric
  Windows taskbar overlay.
- Native Claude and Codex version checks run through the same sanitized environment allowlist as
  live sessions, including opt-in proxy variables without inheriting parent secrets
- Ships an installer that actually starts: the NSIS and MSI bundles carry the daemon and probe as
  sidecars, and `scripts/verify-installer.ps1` installs into a scratch prefix, launches the
  installed binary on an isolated display, asserts the window and the daemon pipe, then uninstalls
- Tails each live session's transcript for its own session id, last message and real cost, reading
  only the bytes appended since the last poll
- Lands a session's work into its repository through a serialised gate that refuses whole — naming
  the specific condition — rather than half-applying
- Reads a per-repository environment lease (`.terminalai/environment.toml`) covering untracked
  config, a docker compose project prefix and a Postgres database cloned per session
- Exposes the fleet to an MCP client over stdio, read-only unless a write token and opted-in
  sessions are supplied
- Keeps a bounded in-memory ring per session over a rotating on-disk log, so history outlives the
  ring and survives a daemon restart
- Gives a session its own Git worktree and branch on request, cleaned up with the row and never at
  the cost of unmerged commits
- Breaks fleet spend down by agent, project folder and session, counting unpriced sessions apart
  rather than as zero
- Sends one prompt to many sessions, reporting each separately and refusing those waiting on a
  permission decision
- Reads launch templates a repository declares about itself (`.terminalai/templates.toml`)
- Registers a root once and turns every Git repository under it into a launch target, with a view
  of which ones still have open roadmap items
- Queues prompts against a busy session, advancing on the reported status and pausing rather than
  answering a permission prompt blind
- Runs one stored prompt across many projects, flagging repositories with uncommitted changes
  instead of launching into them
- 520 default Rust tests over agent identification and resolution against an injected filesystem,
  the flag mapping, real-pty boundary and blocking exit wait, supervision state machine, registry,
  diagnostics, review aggregation and reviewed-mark expiry, the land gate against real
  repositories, environment leases, transcript tailing, the MCP boundary, cost model and vendored
  price table, external-session discovery, daemon protocol and frame bounds, presets, launch golden
  fixtures, atomic file recovery, corrupt-store quarantine, deterministic fleet ordering, prompt
  safety and fleet-row model, per-session worktrees against real repositories, the two-tier
  scrollback store, repository-declared templates, the broadcast eligibility rule, project
  discovery and roadmap scanning against this machine's own repositories, the prompt queue's state
  machine and the work queue's dirty-tree refusal, the fleet spend ledger and its admission ceiling,
  memory-aware admission and job limits, agent authentication state, the bounded session
  archive and the leftover-checkout survey; 523 with the opt-in
  app-server transport enabled, plus 287 frontend tests (`npm --prefix web test`)

`Roadmap_Blocked.md` records what is waiting on something external. The experimental
Codex app-server adapter is available only when the daemon is built with the explicit
`codex-app-server` feature; the default daemon continues to use hooks and the PTY path. The named-pipe
daemon keeps live sessions independent of the window; see
[ROADMAP.md](ROADMAP.md).

The daemon writes a versioned, human-readable session store to
`%LOCALAPPDATA%\TerminalAI\sessions.json` and raw replay tails to its adjacent
`sessions.json.scrollback` directory. If the file is unreadable or has an unsupported schema,
the daemon moves it to a timestamped `sessions.corrupt-...json` quarantine and starts with an empty
fleet; the window shows the quarantine path in a dismissible banner. Reconnecting the window reattaches to live rows and
replays bounded scrollback; rows recovered after a daemon restart remain stopped until the operator
chooses native resume or archive.

Session metadata persists after a 200 ms quiet period and at least once per second during sustained
output, so a busy fleet cannot indefinitely delay its newest durable state.

Each session retains the latest 64 status transitions with their timestamp and source, and the
terminal panel's diagnostics view explains why the focused row is in its current state. Daemon and
desktop-shell panics enter the structured rolling log under
`%LOCALAPPDATA%\TerminalAI\logs\`; fourteen daily files are retained, and the
focused terminal's log panel keeps the latest 256 records in memory.

Fleet rows expose every status with a distinct glyph and visible text label, remain keyboard
navigable, announce live status changes to screen readers, and use high-contrast tokens in both
the dark and OS light palettes. Reduced-motion and forced-colors preferences are respected. The
focused terminal toolbar includes an explicit opt-in screen-reader mode; xterm's mode changes its
right-click copy/paste behavior, so it is never enabled implicitly.

The top-bar **Check updates** action reads the latest GitHub release metadata only. It reports a
new version when one exists but never downloads, launches, or installs an update automatically.

Attention notifications are deduplicated per session and status, grouped by repository, retracted
when the agent proceeds, and quiet during startup or the first seconds of a tool call.

The daemon admits three live processes by default. Set `TERMINALAI_MAX_LIVE_SESSIONS` to change the
cap and `TERMINALAI_DEFAULT_BUDGET_USD` to change the default Claude `--max-budget-usd` (or `none` to
disable it).

A per-session budget bounds one agent; nothing bounded the fleet, so twenty sessions each obeying a
$5 cap could spend $100 with every individual limit reporting itself satisfied.
`TERMINALAI_SPEND_CEILING_USD` sets a fleet-wide ceiling over a rolling window
(`TERMINALAI_SPEND_WINDOW_HOURS`, 24 by default); reaching it stops anything new from starting and
never touches a session that is already running. The ledger is persisted with the session store, so
restarting the daemon does not clear the window.

**Limits** in the toolbar edits all of this against the running daemon — live sessions, the default
session budget, the spend ceiling and window, the fleet memory budget, the per-session memory cap
and process count — and applies it without a restart. Environment variables remain the boot default,
and the dialog names the ones it started from. `terminalai-probe limits [--max-live N]` drives the
same two requests headlessly. Because only Claude takes a per-session
`--max-budget-usd`, the header says which agents a budget actually binds rather than implying a hard
stop the whole fleet does not have.

The fleet header shows live/queued counts, how many sessions a provider is currently
rate limiting, and when the soonest quota window reopens. Spend is derived from each session's
transcript and renders an em dash until at least one session reports one — a computed-looking
`$0.00` would be worse than saying so — with a tooltip naming the price table it was computed
against.

Prices come from a commit-pinned snapshot of LiteLLM's `model_prices_and_context_window.json`
(MIT), vendored at `crates/terminalai-core/pricing/model-prices.json` and embedded in the binary.
Nothing is fetched at runtime, so an offline machine prices identically to a connected one. The
model expresses both cache-write tiers (5-minute and 1-hour, billed at 1.25x and 2x base input),
the regional-inference premium and the priority-speed premium, because real transcript records on
this machine carry all of them.

The fleet header also shows counts for every session state. Compact rows keep the status glyph,
session name, repository, agent, tool progress, restart count, dwell time and last output line on
one 28px line; use **Wide** to reveal model, reasoning effort, branch, ports, the spelled-out
status and reported cost on a second line. Branch is read from the session directory's Git HEAD
at launch and refreshed from hook events; it is an em dash outside a repository or on a detached
HEAD rather than a guess. Tool progress is populated from the agent's own plan — Claude Code's
`TodoWrite`, Codex's `update_plan` — and is an em dash when the agent exposes no countable plan. Press `/` anywhere outside a text field to focus the fleet
filter.

Each launch can reserve a deterministic block of service ports and, only when the operator supplies
them, run setup and teardown shell commands from the project directory. These hooks are local code
execution with the same trust as the operator's account; they are not agent-sandboxing or a remote
service boundary. Hooks and the agent receive `TERMINALAI_SESSION_ID`,
`TERMINALAI_PORTS`, `TERMINALAI_PORT_BASE`, and the first port as `PORT`.

Sessions started outside TerminalAI — from a terminal, an IDE, or another tool — appear in a
separate **Elsewhere on this machine** panel, read from Claude Code's own per-PID session registry
with `claude agents --json` as the reconciliation fallback. Those rows are deliberately actionless:
TerminalAI owns none of their processes, so it offers nothing it cannot perform. Identity is
`(pid, procStart)`, which survives PID reuse. A registry that cannot be read reports "unknown",
never "idle" — reporting idle from the absence of a signal is the failure mode this whole surface
exists to avoid.

The Review view is a daemon-owned, read-only Git diff surface across all sessions with changes.
It ranks entries by file and line review cost, shows additions/deletions and conflict markers, caps
diff payloads at 128 KiB per session, and lets the operator mark a session reviewed. It never stages,
resolves, or commits changes.

## Requirements

- Rust 1.88+ (the true floor; see Build for how to re-derive it)
- Claude Code and/or Codex CLI installed
- Windows 10 1809+ for ConPTY (macOS and Linux use the platform pty)

Verified against Claude Code 2.1.170 and codex-cli 0.146.0 on Windows 11 26100.

## Build

```powershell
cargo build --release
cargo test
cargo test --workspace --all-features  # includes the experimental app-server adapter
npm --prefix web test
cargo deny --target x86_64-pc-windows-msvc check advisories bans licenses sources

# Build unsigned Windows NSIS and MSI installers (from the repository root).
cargo tauri build --ci --no-sign --bundles nsis,msi -- --manifest-path Cargo.toml

# Release gate: install into a scratch prefix, launch, assert the window and the
# daemon, then uninstall. Non-zero exit means do not publish.
pwsh -NoProfile -File scripts/check-cross-targets.ps1
pwsh -NoProfile -File scripts/verify-installer.ps1

# Re-derive the MSRV floor after any dependency bump. The workspace rust-version
# must be at least what this prints, or the badge promises a toolchain that cannot
# build the tree.
cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc |
  ConvertFrom-Json |
  Select-Object -ExpandProperty packages |
  Where-Object rust_version |
  Sort-Object { [version]$_.rust_version } -Descending |
  Select-Object -First 5 name, version, rust_version
```

Note that plain `cargo build --release` is only for the probe, daemon and tests: the
`terminalai.exe` it produces is a development shell that navigates to the Vite dev server
(`http://127.0.0.1:5173`) and shows `ERR_CONNECTION_REFUSED` when no dev server is running. The
shippable app binary comes from `cargo tauri build`, which enables Tauri's `custom-protocol`
feature and embeds the built frontend.

The Tauri build stages `terminalai-daemon.exe` and `terminalai-probe.exe` as sidecars and runs
the Vite frontend build automatically (`scripts/prebuild.ps1`). The app spawns the daemon from
its own directory, so a bundle without those sidecars installs cleanly and then exits before
drawing a window — `scripts/verify-installer.ps1` exists to catch exactly that, because a build
tree always has the sibling executables and never reproduces the failure.

The installer is written to
`target/release/bundle/nsis/` and `target/release/bundle/msi/`; both artifacts are intentionally
unsigned. Windows SmartScreen may warn on first launch: choose **More info**, then **Run anyway**
only if the installer came from your verified build or release source. The WebView2 dependency uses
the Evergreen `downloadBootstrapper` mode instead of bundling a fixed runtime.

The Codex app-server stdio transport is deliberately opt-in:

```powershell
cargo test -p terminalai-daemon --features codex-app-server
```

It preserves unknown JSON-RPC notifications and exposes typed status, token-usage, approval,
steer and interrupt messages without changing the default daemon process.

## Probe

The headless harness. It exists because a GUI cannot be unit-tested against a real agent
process, so everything that touches the machine is exercised here first.

```powershell
# Where are the agents?
terminalai-probe resolve

# Drive the daemon headlessly; each --json command emits one stable JSON object.
terminalai-probe list --json
terminalai-probe start claude --cwd . --prompt "run the tests" --json
terminalai-probe status s0001 --json
terminalai-probe send s0001 "focus on failing tests first" --json
terminalai-probe stop s0001 --json
terminalai-probe shutdown

# What would this launcher choice actually run?
terminalai-probe preview claude --model opus --effort xhigh --permission plan
terminalai-probe preview codex --model gpt-5.1-codex --effort high --sandbox workspace-write

# Run it for real on a pseudo-console.
terminalai-probe spawn claude --raw --version

# Print the installed runtime's model/effort catalog and feature names.
terminalai-probe capabilities codex --json

# Run anything on a pseudo-console — isolates "the pty is broken" from
# "this agent is behaving oddly under a pty".
terminalai-probe exec cmd.exe /c "echo hello"

# Measure console churn and marker input latency on Windows (the default is 8 sessions).
terminalai-probe hygiene --sessions 8 --json --output .\hygiene.json

# Deliver one Claude/Codex hook payload without a browser-reachable listener.
echo '{"session_id":"...","hook_event_name":"Notification","notification_type":"permission_prompt"}' |
  terminalai-probe hook claude

# Preview, inspect, install, or remove the explicitly managed agent hooks.
terminalai-probe hooks preview claude --executable .\target\release\terminalai.exe
terminalai-probe hooks status claude --executable .\target\release\terminalai.exe
terminalai-probe hooks install claude --executable .\target\release\terminalai.exe
terminalai-probe hooks remove claude --executable .\target\release\terminalai.exe
```

The Windows hygiene probe was run on 2026-08-03 with eight sessions inside the required isolated
non-input desktop. It samples `ConsoleWindowClass` handles on that desktop; when Windows keeps a
console host non-enumerable there, it falls back to the delta of `conhost.exe` process snapshots.
Input latency is a marker round trip over redirected stdin/stdout, so it does not inject keyboard
input into any desktop. Each row contains eight latency samples; the console count is a creation
churn count, so host cleanup races can make it exceed the requested session count.

| launch mode | console-window/host creations | min input (ms) | median (ms) | max (ms) |
|---|---:|---:|---:|---:|
| TerminalAI supervised ConPTY | 0 enumerable windows | 0.1565 | 0.1694 | 0.1810 |
| Normal `CREATE_NEW_CONSOLE` launch | 10 `conhost.exe` hosts | 0.2281 | 0.2632 | 56.3870 |

`shutdown` asks the daemon to tear down its owned sessions and exit cleanly. The daemon also
handles Windows console-close and shutdown signals. Protocol compatibility is negotiated on the
stable local endpoint, so an older running daemon is reported with its PID instead of causing the
desktop shell to start a second owner of the fleet.

Hook installation is opt-in and only owns entries carrying `--terminalai-managed`; unrelated
Claude handlers and Codex `notify` commands are preserved. Use `--config <path>` to inspect or
modify a disposable settings file before touching the user-level config.

## What the launcher maps to

Verified flags, not guesses.

| Control | Claude Code | Codex |
|---|---|---|
| Model | `--model` | `--model` |
| Effort | `--effort <runtime value>` | `--config model_reasoning_effort="…"` |
| Project folder | process cwd | `--cd` |
| Extra writable dirs | `--add-dir` | `--add-dir` |
| Permission | `--permission-mode default\|plan\|acceptEdits\|bypassPermissions` | Plan: `--config collaboration_mode.mode="Plan"`; otherwise `--ask-for-approval on-request\|untrusted\|never` |
| Sandbox | — | `--sandbox read-only\|workspace-write\|danger-full-access` |
| Session label | `--name` | TerminalAI-side |
| Config profile | — | `--profile` |
| Resume last | `--continue` | `resume --last` |
| Resume by id | `--resume <id>` | `resume <id>` |
| Fork | `--resume <id> --fork-session` | `fork <id>` |
| Spend cap | `--max-budget-usd` | — |
| Web search | — | `--search` |

Options an agent cannot express are **refused, not dropped**. Asking for a read-only sandbox and
silently getting an unsandboxed Claude session is the kind of thing that costs you a repo. Prompts
are placed after an explicit `--` option terminator, so pasted text beginning with `-` remains text.
The `extra_args` escape hatch is trusted-input-only and is never populated from prompt text.

## Layout

```
crates/
  terminalai-core/    agent resolution, flag mapping, ConPTY supervision, fleet model
  terminalai-daemon/  named-pipe control plane and the only process that owns live sessions
  terminalai-app/     Tauri shell, launcher commands, preset store and app icon
  terminalai-probe/   headless harness for everything that touches the machine
web/                   Vite frontend, Catppuccin fleet surface and xterm renderer
```

The core registry supervises an injected `AgentDomain` through an object-safe `AgentSession`
contract. `LocalPtyDomain` is the default ConPTY implementation; a remote transport can provide
the same output, input, resize and lifecycle seam without exposing a local process handle to the
registry.

The web renderer and daemon share the Fluent catalog at `web/src/i18n/terminalai.ftl`. Rust
validates and formats it through `terminalai_core::Catalog`; the renderer applies the same
message IDs and uses the browser's `Intl` plural and relative-time formatters for counts and
dwell labels. Status diagnostics cross the daemon boundary as reason kinds plus arguments, so
translated prose never becomes a wire-format dependency.

Hook installation covers the lifecycle events that change a fleet row, including prompt submission,
tool failures, permission decisions, subagents, compaction and session end. Managed Claude and
Codex hooks use the command adapter, and each supervised session carries a random hook secret only
in its own agent environment; the daemon refuses cwd-based identity and resume-id rebinding.
The daemon also exposes an ephemeral loopback HTTP endpoint for explicit callers: its bearer token,
exact Host allowlist and Origin rejection are only the transport gate, and a caller must provide
the matching per-session header before a row can change. Unknown hook event names are retained in
the diagnostics stream.

The launcher discovers model and reasoning-effort options from the resolved binaries instead of
shipping a stale allowlist. Codex is queried through `model/list` and `codex features list`; Claude
reports its active model and protocol capabilities through `system/init`. Results are cached for the
resolved executable and its version banner, invalidated when that version changes, and surfaced as
runtime datalists. A user-entered model or effort that is not advertised remains free text and is
passed through with a warning.

## Licence

MIT — see [LICENSE](LICENSE).
