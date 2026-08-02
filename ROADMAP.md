# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; items are struck through when shipped.

## v0.2.0 — the window

- [ ] Tauri 2 shell: Rust core + WebView2 frontend, Catppuccin Mocha, dark by default
- [ ] Launcher dialog — agent, model, effort, permission, sandbox, folder picker, extra dirs,
      resume/fork, spend cap, initial prompt; live "what will run" preview from
      `ResolvedCommand::preview()`
- [ ] Presets — save a launcher configuration by name, launch it in one click
- [ ] Fleet list — 28px rows: status dot, agent badge, model+effort, folder, dwell time, last line
- [ ] Focused terminal pane via xterm.js, wired to `PtySession` read/write/resize
- [ ] Session registry in the Rust core; the frontend holds no session state

## v0.3.0 — knowing what the fleet is doing

- [ ] Hook bus: local listener; register Claude Code `SessionStart` / `Stop` / `Notification` /
      `PreToolUse` / `PostToolUse` hooks that post session state
- [ ] Transcript tailing — `~/.claude/projects/<slug>/*.jsonl` and Codex session rollouts, for
      last message, native session id, token and cost accounting
- [ ] Status inference fallback for sessions started outside TerminalAI
- [ ] Pin up to three sessions to keep live grids; split view
- [ ] Windows toast on `NeedsYou`, with click-to-focus

## v0.4.0 — many sessions, one operator

- [ ] Daemon: sessions survive closing the window; named-pipe IPC; reattach on relaunch
- [ ] Scrollback to disk with a bounded in-memory ring per session
- [ ] Git worktree per session, created and cleaned up from the launcher
- [ ] Broadcast a prompt to a selected set of sessions
- [ ] Cost and token rollup across the fleet
- [ ] Filter and group the list by folder, agent, status

## v0.5.0 — beyond the terminal

- [ ] ACP transport as an alternative to the pty, for a compact native chat view
      (`@zed-industries/claude-code-acp`, `agentclientprotocol/codex-acp`)
- [ ] `codex app-server` JSON-RPC transport
- [ ] Session templates per repo, read from the repo itself

## Open questions

- Does Codex accept `model_reasoning_effort="max"`? Claude does; the launcher currently offers
  Codex only `low|medium|high|xhigh`. Needs an empirical check against a live session.
- Codex has no plan mode. The launcher refuses it rather than quietly substituting a read-only
  sandbox — confirm that is the behaviour that survives contact with real use.
