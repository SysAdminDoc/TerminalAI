# Changelog

All notable changes to TerminalAI are recorded here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

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
