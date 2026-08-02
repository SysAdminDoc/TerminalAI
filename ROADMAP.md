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
      — *2026-08-02 research: resize must NOT follow the splitter. See R-11; PTY width is canonical and fixed at
      spawn. Also a single reused instance, not one per session — see R-12.*

## v0.3.0 — knowing what the fleet is doing

- [ ] Hook bus: local listener; register Claude Code `SessionStart` / `Stop` / `Notification` /
      `PreToolUse` / `PostToolUse` hooks that post session state
      — *2026-08-02 research: a bare loopback HTTP listener is browser-reachable and DNS-rebinding-attackable.
      Harden per R-07. Claude exposes 30 hook events and `Notification.type` is the blocked-state signal (R-08);
      Codex has its own hooks table plus a `notify` key (R-09).*
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

- [ ] Daemon: sessions survive closing the window; named-pipe IPC; reattach on relaunch
      — *2026-08-02 research: REPRIORITIZED — this must land before v0.2, not after. VS Code moved node-pty out of
      the renderer (microsoft/vscode#117265) only after pty crashes took down windows; every surveyed project that
      started in-process paid to move later. The new `terminalai-core::registry` should move behind this boundary
      rather than living in the Tauri backend. Harden the pipe per R-07.*
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
- [ ] `codex app-server` JSON-RPC transport
      — *2026-08-02 research: 90 methods / 68 notifications / 10 approval requests; `thread/status/changed` and
      `thread/tokenUsage/updated` are the signals worth having. Keep behind a flag — see R-19.*
- [ ] Session templates per repo, read from the repo itself

## Open questions

- Does `claude --resume <id>` restore enough context that hibernation is transparent to the user?
  Blocks R-06. Needs live validation.

## Research-Driven Additions

Added 2026-08-02 from `RESEARCH.md`. IDs R-01…R-33; the next researcher continues from R-34.

### P0

- [ ] R-07 · P0 — Harden the hook/control transport before it ships
  Why: a bare loopback HTTP listener is reachable by any page the user visits, and DNS rebinding defeats `Origin` checks.
  Evidence: CVE-2025-66414 (MCP TypeScript SDK); GitHub Security Lab "localhost dangers"; CyberCX on named-pipe squatting.
  Touches: new `crates/terminalai-daemon`, the v0.3 hook-bus item
  Acceptance: control plane is a named pipe created with `first_pipe_instance(true)` and a DACL limited to the current user SID + SYSTEM, peer PID checked via `GetNamedPipeClientProcessId`, and no use of `ImpersonateNamedPipeClient`. If an HTTP endpoint is retained for Claude's native `type: "http"` hooks, it binds `127.0.0.1` and enforces all three of a literal `Host` allowlist, a startup-generated bearer token carried in the hook's `headers`, and an `Origin` rejection.
  Complexity: M

- [ ] R-23 · P0 — Pin Tauri ≥ 2.11.5 and enable CSP when the shell lands
  Why: CVE-2026-42184 is a Windows-specific `is_local_url()` subdomain bypass letting remote pages invoke local-only IPC; Tauri's CSP is opt-in and absent unless configured.
  Evidence: GHSA-7gmj-67g7-phm9 (CVSS 6.1, affects ≥2.0 ≤2.11.0); https://v2.tauri.app/security/csp/
  Touches: `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/capabilities/`
  Acceptance: dependency is `tauri = "2.11.5"` (never `"2"`), `app.security.csp` is set, and capabilities grant only the permissions actually used.
  Complexity: S

### P1

- [ ] R-08 · P1 — Blocked-state detection from Claude Code's `Notification` hook
  Why: this is the single signal the whole fleet list depends on, and every competitor that infers it from terminal output has a top-ranked "wrong status" bug.
  Evidence: Claude Code exposes 30 hook events; `Notification` carries `type: "permission_prompt" | "idle_prompt"`. Counter-examples: ccmanager#227, cmux#1027, claude-squad#216.
  Touches: `crates/terminalai-core/src/session.rs`, daemon hook endpoint
  Acceptance: `permission_prompt` → needs-approval, `idle_prompt` → awaiting-input, `Stop` → idle, correlated by `session_id`; hooks registered `async: true` so they never stall the agent.
  Complexity: M

- [ ] R-09 · P1 — Blocked-state detection for Codex
  Why: Codex has a hooks table including `PermissionRequest`, which the closest competitor believes does not exist and therefore cannot show "waiting" for Codex at all.
  Evidence: https://developers.openai.com/codex/config-schema.json (`[hooks]` keyed by `PreToolUse`, `PermissionRequest`, `PostToolUse`, `SessionStart`, `SessionEnd`, `Stop`, …); the gap is workmux#197.
  Touches: daemon hook endpoint, Codex config writer
  Acceptance: a Codex session blocked on an approval renders needs-approval; `notify = ["<terminalai-exe>"]` works as a zero-config fallback when hooks are unavailable.
  Complexity: M

- [ ] R-10 · P1 — Deduplicate token accounting on `requestId`
  Why: consecutive transcript records share one `requestId` and repeat identical `usage`; naive summing double-counts spend.
  Evidence: verified against live `~/.claude/projects/*/*.jsonl`, 2026-08-02.
  Touches: transcript tailer
  Acceptance: a fixture with repeated `requestId` records sums once; cost is derived from `usage` × a versioned price table, since cost is absent from the JSONL.
  Complexity: S

- [ ] R-11 · P1 — Fix PTY width at spawn; never resize on layout change
  Why: agent TUIs hard-wrap and multiplexers deliberately do not reflow, so resizing on a splitter drag forces a full redraw that corrupts the output being parsed for status.
  Evidence: wezterm#14, wezterm#5016, xterm.js#1864, codex#18575; RESEARCH.md "Architecture".
  Touches: `crates/terminalai-core/src/pty.rs`, focused-pane component
  Acceptance: each session has a canonical width chosen at spawn; the focused pane scrolls or scales rather than renegotiating; `resize()` fires only on explicit user action.
  Complexity: M

- [ ] R-12 · P1 — One terminal renderer, Rust-side grids for the rest
  Why: Chromium caps live WebGL contexts at ~16 and silently drops the oldest, so N live xterm.js instances cannot work.
  Evidence: xterm.js#4379 (open, no fix); xterm.js#791 (buffer memory); `@xterm/xterm` 6.0 removed `addon-canvas`.
  Touches: `crates/terminalai-core` (new `vte`-based grid), focused-pane component
  Acceptance: exactly one `Terminal` instance plus one per pinned pane; switching focus does `reset()` + replay from the Rust ring buffer; background sessions hold a `vte` grid only.
  Complexity: L

- [ ] R-13 · P1 — Real supervision state, separate from raw I/O
  Why: `SessionStatus` conflates "what the agent is doing" with "is this session healthy", so a crash-looping session is indistinguishable from a busy one.
  Evidence: systemd `Restart`/`StartLimitBurst`/`RestartSteps`; Docker `start_period`/`retries`; RESEARCH.md "Refactor candidates".
  Touches: `crates/terminalai-core/src/session.rs`, `pty.rs`, `registry.rs`
  Acceptance: a session exposes `phase` (starting|idle|working|awaiting-input|needs-approval|backoff|failed|resurrectable), `health`, `restarts`, `last_exit_code`, `backoff_until`, `state_since`, `pid`, `resume_id`; restart backoff is exponential and gives up into a terminal failed state.
  Complexity: M

- [ ] R-14 · P1 — Three-tier restore ladder
  Why: no design can re-parent a live process, and pretending otherwise loses work; both target CLIs resume natively, which makes the middle tier unusually strong here.
  Evidence: Zellij session resurrection (commands are never auto-run); tmux-resurrect program whitelist; VS Code `persistentSessionReviveProcess`.
  Touches: daemon, session store
  Acceptance: reattach (process alive, replay bounded scrollback) → revive (`claude --resume <id>` / `codex resume <id>`, offered as an explicit per-row action, never automatic) → archive (layout + cwd + command only); session state serializes to a human-readable file on a background thread.
  Complexity: L

- [ ] R-15 · P1 — Approvals inbox with in-list replies
  Why: the highest-value unbuilt feature in the survey; today answering one prompt costs a full context switch into a pane.
  Evidence: claude-squad#312 exists only as the author's private fork; octomux ships a cross-session permission inbox; "which agent needs me" is the #3 recurring community pain.
  Touches: fleet list component, daemon input path
  Acceptance: a filtered view shows only sessions awaiting input or approval; the user can answer or send a short reply from the row without focusing the session, including bracketed paste; per-row buttons rather than bare keystrokes, per the repo's no-shortcuts rule.
  Complexity: L

- [ ] R-16 · P1 — Notification lifecycle: dedupe, auto-retract, grace periods
  Why: a stale "needs input" marker is worse than none, and per-event toasts across 20 sessions are unusable.
  Evidence: PagerDuty `dedup_key` + auto-resolve; Slack per-channel batching; Docker `start_period`; notification fatigue named as the failure mode of the obvious fix.
  Touches: notification subsystem, `session.rs`
  Acceptance: each attention event carries a dedup key, self-retracts when the agent proceeds, is grouped by repo, and is suppressed during a per-state grace window (startup, known-long tool calls).
  Complexity: M

- [ ] R-17 · P1 — Concurrency admission control and per-session budgets
  Why: nothing today stops the fleet exceeding RAM or plan quota; this is the failure every vendor monetizes and no OSS tool guards.
  Evidence: measured 509 MB/session; amirlehmam/wmux#139 (crash loop → 251 processes, 3.3 GB); HN 47221592 (parallel agents exhaust Max limits "in under an hour"); Devin caps its $20 tier at 10 concurrent sessions.
  Touches: daemon scheduler, launcher
  Acceptance: a configurable max-live-sessions with a queue for the overflow; a default `--max-budget-usd` per session; the fleet header shows live/queued counts and aggregate spend.
  Complexity: M

- [ ] R-06 · P1 — Session hibernation and rehydration
  Why: the only lever that makes many tracked sessions affordable — most are idle, and idle sessions cost the same 509 MB as busy ones.
  Evidence: measured RSS and ~250–500 ms cold start (2026-08-02); both CLIs support resume. Blocked on the first Open Question.
  Touches: daemon, session store, `launch.rs`
  Acceptance: an idle session past a threshold is parked (process exited, transcript and resume id retained, row shows hibernated) and rehydrates on focus via the resume path with scrollback replayed from disk.
  Complexity: L

- [ ] R-18 · P1 — Non-destructive state reconciliation
  Why: a failed backend query must never be read as "the session is gone".
  Evidence: workmux#209 — a failed tmux query deletes agent state rather than marking it unknown.
  Touches: daemon reconciliation loop
  Acceptance: reconciliation can only move a session to unknown; deletion requires positive confirmation that the process is gone; a test simulates a query failure and asserts no state loss.
  Complexity: S

### P2

- [ ] R-19 · P2 — `codex app-server` adapter behind a feature flag
  Why: the only path to steering (queue work, interrupt, auto-answer approvals) that hooks structurally cannot provide.
  Evidence: 90 methods / 68 notifications / 10 approval requests; `thread/status/changed`, `thread/tokenUsage/updated`, `turn/steer`, `turn/interrupt`. Schema is generated per build via `codex app-server generate-json-schema`.
  Touches: new transport crate
  Acceptance: the daemon's event model is a superset of hook events and JSON-RPC notifications, so the adapter is additive; the flag defaults off while the interface is experimental.
  Complexity: L

- [ ] R-20 · P2 — Scriptable headless control plane
  Why: every OSS peer is GUI-only and every vendor charges for an API; it is also how TerminalAI becomes usable from the user's own automation.
  Evidence: Conductor's API is Pro-only; Warp's analytics API is Enterprise; RESEARCH.md paywall analysis item 5.
  Touches: `crates/terminalai-probe` (promote to a real CLI), daemon pipe protocol
  Acceptance: `terminalai list|start|stop|send|status --json` drives the daemon over the named pipe with stable JSON output; the GUI uses the same protocol and holds no privileged path.
  Complexity: M

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

- [ ] R-24 · P2 — Consented, reversible hook installation
  Why: writing to the user's global agent config without asking is a trust violation, and orphaned hook entries break the user's CLI after uninstall.
  Evidence: a competitor auto-injects its hook into `~/.claude/settings.json`; Claude Code has a `disableAllHooks` kill switch worth respecting.
  Touches: settings writer, onboarding
  Acceptance: hook installation is explicit, shows the exact JSON to be written, is idempotent, and can be fully removed; TerminalAI detects and reports when its own hooks are missing or stale.
  Complexity: S

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

- [ ] R-30 · P2 — Daemon/GUI version-skew handling
  Why: a long-lived daemon plus a frequently rebuilt GUI guarantees mismatched pairs during development and upgrades.
  Evidence: the daemon split makes this a new failure class; wmux#659 shows how an unversioned control protocol fails confusingly (broadcasts multiplexed onto the RPC socket, read as responses).
  Touches: daemon protocol handshake
  Acceptance: the handshake exchanges protocol versions, an incompatible GUI refuses to connect with an actionable message, and the daemon can be restarted without losing session records.
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
