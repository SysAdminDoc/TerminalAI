# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; completed items are removed.

## v0.3.0 — knowing what the fleet is doing

- [ ] Transcript tailing — `~/.claude/projects/<slug>/*.jsonl` and Codex session rollouts, for
      last message, native session id, token and cost accounting
      — *2026-08-02 research: slug = cwd with `\`, `:`, `.` → `-`; filename stem IS the session UUID. Use
      `ai-title` for the row label and `last-prompt` for context. Codex rollouts live at
      `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO>-<uuid>.jsonl`. Cost is NOT in the JSONL — derive it, and
      dedupe on `requestId` first (see R-10).*
- [ ] Status inference fallback for sessions started outside TerminalAI
      — *2026-08-02 research: when no hook is installed for an agent, render a visible degraded state. Never
      report idle from absence of signal — that is the exact failure mode in ccmanager#227 and cmux#1027.*
- [ ] Pin up to three sessions to keep live grids; split view
- [ ] Windows toast on `NeedsYou`, with click-to-focus
      — *2026-08-02 research: an unpackaged Win32 app cannot raise a toast without a Start Menu shortcut carrying
      `System.AppUserModelID`. Use `tauri-winrt-notification`; click-to-activate additionally needs a COM activator
      registered under `HKCU\Software\Classes\CLSID\{...}\LocalServer32`, which that crate does not do. Pair with
      R-16 so toasts self-retract.*

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

## Research-Driven Additions

Added 2026-08-02 from `RESEARCH.md`. IDs R-01…R-33; the next researcher continues from R-34.

### P1

### P2

- [ ] R-21 · P2 — Fleet row information design
  Why: the row is the product; k9s is the best-studied case of supervising many similar things at a glance.
  Evidence: k9s pod view — compound fractions (`READY 1/1`), a first-class `RESTARTS` column, relative age, glyph decorators, phase-derived colour, `/` filter, and separate aggregate views rather than a wider table. agent-deck's status sigils (`!` `@` `#` `$`).
  Touches: fleet list component
  Acceptance: rows carry status glyph, agent, repo/branch, dwell timer, tool progress as a fraction, restart count, and an ellipsized last line; model/effort/cost live in an expanded "wide" mode; a header strip shows counts by state; `/` filters.
  Complexity: M

- [ ] R-22 · P2 — Per-worktree environment state
  Why: git isolation without port and service isolation still collides; this is universally unserved and the answer everywhere today is "write your own script".
  Evidence: HN 46424131 ("none of them mention databases… I would need ten different copies"); claude-squad#260 requests a worktree env-setup hook with port isolation.
  Touches: worktree manager, session config
  Acceptance: a per-session setup/teardown hook plus deterministic port allocation, with allocated ports visible on the row.
  Complexity: M

- [ ] R-33 · P2 — Unified review surface across sessions
  Why: the most recurrent community complaint by a wide margin is that human review, not agent throughput, is the bottleneck — a fleet manager that increases agent output without addressing review makes the problem worse.
  Evidence: HN 45486217, 45531694, 46424868 ("converting typing time into reading time, which is usually worse"); 91% increase in PR review time and "code began merging unread" (The New Stack). Commercial precedent: Warp's Code Review Panel, Conductor's dedicated PR page.
  Touches: new review view, worktree manager
  Acceptance: one view aggregates the pending diff of every session with per-session file counts and line deltas, ordered by review cost; a session can be marked reviewed; conflict markers are surfaced rather than auto-resolved.
  Complexity: L

- [ ] R-25 · P2 — Test harness for the machine-facing layer
  Why: both bugs found on 2026-08-02 (the ConPTY DSR stall and EOF-based exit detection) were reachable by an automated test that did not exist.
  Evidence: `CLAUDE.md ## Learned`; RESEARCH.md "Test gaps".
  Touches: `crates/terminalai-core/tests/`
  Acceptance: an integration test spawns `cmd.exe /c echo` (and `/bin/echo`) on a real pty and asserts output and exit code; golden-file tests pin the argument vector per agent per version and fail loudly when a CLI changes its flags.
  Complexity: M

- [ ] R-26 · P2 — Diagnostics and structured logging
  Why: a supervisor that cannot explain why it thinks a session is idle is unfixable in the field.
  Evidence: repo convention requires a log panel and crash log; status misattribution is the field's dominant bug class.
  Touches: daemon, GUI diagnostics panel
  Acceptance: every status transition records its source (hook event, transcript record, process exit) with a timestamp, viewable per session; a crash log is written on panic; a "why is this session marked X" view exists.
  Complexity: M

- [ ] R-27 · P2 — Accessibility floor for the fleet list
  Why: status is currently encoded as a colour token, which fails for colour-blind users and any high-contrast theme.
  Evidence: `SessionStatus::colour()` returns theme colour names as the only status encoding.
  Touches: `crates/terminalai-core/src/session.rs`, fleet list component
  Acceptance: every status has a distinct glyph and text label independent of colour; contrast meets WCAG AA in both themes; the list is navigable and announced by a screen reader; motion respects `prefers-reduced-motion`.
  Complexity: M

- [ ] R-28 · P2 — Distribution and update flow
  Why: unsigned Windows distribution has a known SmartScreen path and a WebView2 decision that changes installer size by ~180 MB.
  Evidence: https://v2.tauri.app/distribute/windows-installer/ — WebView2 is Evergreen-preinstalled on Win10 21H2+/Win11, so `downloadBootstrapper` is correct; `fixedRuntime` adds ~180 MB.
  Touches: `src-tauri/tauri.conf.json`, release process
  Acceptance: unsigned NSIS/MSI built locally, `downloadBootstrapper` mode, documented SmartScreen "More info → Run anyway" step, and an in-app update check that never auto-installs.
  Complexity: S

- [ ] R-29 · P2 — Versioned session store with a migration path
  Why: session state will outlive its schema, and a daemon that survives GUI upgrades will meet older files.
  Evidence: Zellij versions its session-info directory by release; RESEARCH.md "Architecture".
  Touches: session store
  Acceptance: the store carries a schema version, unknown-newer files are refused rather than misread, and migrations are tested against fixtures from the previous version.
  Complexity: S

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
  Touches: GUI string layer
  Acceptance: user-facing strings live in a resource file with a single locale, dates and durations use locale-aware formatting, and the layout tolerates ~30% string growth.
  Complexity: S
