# TerminalAI

[![version](https://img.shields.io/badge/version-0.1.0-blue.svg)](CHANGELOG.md)
[![license](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](#requirements)
[![rust](https://img.shields.io/badge/rust-1.82%2B-orange.svg)](https://www.rust-lang.org/)

A control surface for running many Claude Code and Codex sessions at once.

Pick the agent, model, effort, permission mode and project folder from a GUI; TerminalAI
launches the real CLI on a pseudo-console and puts it in a dense fleet list. Thirty tracked
sessions fit on one screen because only the ones you are actually looking at get a terminal
grid — the rest are a status row.

## Why another one

Every multiplexer in this space renders **panes**. Panes do not stack: four xterm panes fill a
1440p display. TerminalAI splits the problem in two.

| | Needs | Cost |
|---|---|---|
| **Attention data** — busy? blocked on approval? which model? how long? | ~28px | a ring buffer |
| **Interaction surface** — the actual TUI | ~600px | a terminal renderer |

You only ever type into one session at a time, so only one session pays for a renderer
(plus any you pin). Everything else is a row.

Thirty is a tracked-session target, not a promise to keep thirty agent processes hot. Local
measurements on 2026-08-02 showed roughly 509 MB RSS for Claude Code and 322 MB for Codex per
live process; real work should stay within the machine's memory budget, typically two or three
parallel agents. The daemon can hibernate idle sessions while retaining their rows.

Status comes from agent hooks and transcript tailing — pushed, not polled. Polling a pty on a
timer, which is the common approach, both misses transitions between ticks and burns CPU per
session.

## Status

**v0.1.0 — core and desktop shell built.**

Working today (`terminalai-probe`, headless):

- Resolves the real `claude.exe` / `codex.exe` behind the npm shims
- Builds the exact argument vector for any launcher combination, per agent
- Spawns and drives either agent on a live ConPTY
- Runs a Tauri 2.11.5/WebView2 shell with the Catppuccin fleet list, launcher, presets and one
  reused xterm renderer
- 79 default unit tests over the flag mapping, pty boundary, supervision state machine, registry,
  daemon protocol, presets and fleet-row model; 81 with the opt-in app-server transport enabled

Not built yet: transcript file tailing and transcript-derived status/cost accounting. The experimental
Codex app-server adapter is available only when the daemon is built with the explicit
`codex-app-server` feature; the default daemon continues to use hooks and the PTY path. The named-pipe
daemon keeps live sessions independent of the window; see
[ROADMAP.md](ROADMAP.md).

The daemon writes a versioned, human-readable session store to
`%LOCALAPPDATA%\TerminalAI\sessions.json`. Reconnecting the window reattaches to live rows and
replays bounded scrollback; rows recovered after a daemon restart remain stopped until the operator
chooses native resume or archive.

Attention notifications are deduplicated per session and status, grouped by repository, retracted
when the agent proceeds, and quiet during startup or the first seconds of a tool call.

The daemon admits three live processes by default. Set `TERMINALAI_MAX_LIVE_SESSIONS` to change the
cap and `TERMINALAI_DEFAULT_BUDGET_USD` to change the default Claude `--max-budget-usd` (or `none` to
disable it). The fleet header shows live/queued counts and the aggregate reported spend.

## Requirements

- Rust 1.82+
- Claude Code and/or Codex CLI installed
- Windows 10 1809+ for ConPTY (macOS and Linux use the platform pty)

Verified against Claude Code 2.1.170 and codex-cli 0.146.0 on Windows 11 26100.

## Build

```powershell
cargo build --release
cargo test
cargo test --workspace --all-features  # includes the experimental app-server adapter

# Build the unsigned Windows NSIS installer (from the repository root).
cargo tauri build --ci --no-sign --bundles nsis -- --manifest-path Cargo.toml
```

The Tauri build runs the Vite frontend build automatically. The installer is written to
`target/release/bundle/nsis/` and is intentionally unsigned.

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

# What would this launcher choice actually run?
terminalai-probe preview claude --model opus --effort xhigh --permission plan
terminalai-probe preview codex --model gpt-5.1-codex --effort high --sandbox workspace-write

# Run it for real on a pseudo-console.
terminalai-probe spawn claude --raw --version

# Run anything on a pseudo-console — isolates "the pty is broken" from
# "this agent is behaving oddly under a pty".
terminalai-probe exec cmd.exe /c "echo hello"

# Deliver one Claude/Codex hook payload without a browser-reachable listener.
echo '{"session_id":"...","hook_event_name":"Notification","notification_type":"permission_prompt"}' |
  terminalai-probe hook claude

# Preview, inspect, install, or remove the explicitly managed agent hooks.
terminalai-probe hooks preview claude --executable .\target\release\terminalai.exe
terminalai-probe hooks status claude --executable .\target\release\terminalai.exe
terminalai-probe hooks install claude --executable .\target\release\terminalai.exe
terminalai-probe hooks remove claude --executable .\target\release\terminalai.exe
```

Hook installation is opt-in and only owns entries carrying `--terminalai-managed`; unrelated
Claude handlers and Codex `notify` commands are preserved. Use `--config <path>` to inspect or
modify a disposable settings file before touching the user-level config.

## What the launcher maps to

Verified flags, not guesses.

| Control | Claude Code | Codex |
|---|---|---|
| Model | `--model` | `--model` |
| Effort | `--effort low\|medium\|high\|xhigh\|max` | `--config model_reasoning_effort="…"` |
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
silently getting an unsandboxed Claude session is the kind of thing that costs you a repo.

## Layout

```
crates/
  terminalai-core/    agent resolution, flag mapping, ConPTY supervision, fleet model
  terminalai-daemon/  named-pipe control plane and the only process that owns live sessions
  terminalai-app/     Tauri shell, launcher commands, preset store and app icon
  terminalai-probe/   headless harness for everything that touches the machine
web/                   Vite frontend, Catppuccin fleet surface and xterm renderer
```

## Licence

MIT — see [LICENSE](LICENSE).
