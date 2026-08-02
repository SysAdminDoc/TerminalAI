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

**v0.1.0 — core proven, GUI not built yet.**

Working today (`terminalai-probe`, headless):

- Resolves the real `claude.exe` / `codex.exe` behind the npm shims
- Builds the exact argument vector for any launcher combination, per agent
- Spawns and drives either agent on a live ConPTY
- 25 unit tests over the flag mapping, pty boundary, registry and fleet-row model

Not built yet: the Tauri window, the fleet list, the hook bus, persistence. See [ROADMAP.md](ROADMAP.md).

## Requirements

- Rust 1.82+
- Claude Code and/or Codex CLI installed
- Windows 10 1809+ for ConPTY (macOS and Linux use the platform pty)

Verified against Claude Code 2.1.170 and codex-cli 0.146.0 on Windows 11 26100.

## Build

```powershell
cargo build --release
cargo test
```

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
```

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
  terminalai-probe/   headless harness for everything that touches the machine
```

## Licence

MIT — see [LICENSE](LICENSE).
