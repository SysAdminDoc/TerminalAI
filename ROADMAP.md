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
      — *seed the library from the operator's existing templates: `~/.claude/prompts/research-deep.txt`
      (research new roadmap items — the source of this repo's `RESEARCH.md`) and
      `~/.claude/prompts/roadmap-drain.txt` (drain the roadmap). Both are 6–11 KB of prose, so the
      store must hold multi-KB text and deliver it as a pty write, never as an argv argument or a
      shell-interpolated string — see the native-binary resolution decision in `CLAUDE.md` for why
      `cmd.exe` quoting is not an option.*

## Research-Driven Additions

From `RESEARCH.md`. IDs R-01…R-63; the next researcher continues from R-64.

### P0

### P1

### P2

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

### P1

### P2

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

### P3

- [ ] R-87 · P3 — Expose the fleet as a read-mostly MCP server
  Why: no tool in the survey unifies both vendors' session lists behind one interface, so an agent cannot ask what its siblings are doing — and the read half of that is cheap once R-72 lands.
  Evidence: prior art is spawn-only or single-vendor — agent-dispatch delegates to `claude -p` in other directories, claude-code-mcp is one-shot and single-agent, pal-mcp-server does cross-vendor subagent spawning; Kandev and Paseo expose their own platforms over MCP but not a unified cross-vendor session list. MCP spec 2026-07-28 removed protocol-level sessions and `Mcp-Session-Id` from Streamable HTTP, making the transport stateless and pushing state back to the server. Tool poisoning — malicious instructions in tool metadata, re-executed on every invocation — is the dominant 2026 MCP attack class per the NSA/CISA advisory and Microsoft's June 2026 warning.
  Touches: new crate or daemon subcommand, `crates/terminalai-daemon/`
  Acceptance: a stdio MCP server exposes read tools (list sessions, read status, read a session's last output, read fleet cost) with no gating; any mutating tool (spawn, kill, send) is opt-in per session, requires an out-of-band token, and logs every invocation into the diagnostics timeline; the server refuses to expose transcript content by default. Depends on R-72.
  Complexity: M
