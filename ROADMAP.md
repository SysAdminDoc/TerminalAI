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

## Research-Driven Additions — external survey, 2026-08-02

From the same-day external research pass in `RESEARCH.md` (competitors, community signal, agent-platform APIs,
dependency and CVE review). IDs R-64…R-87 do not overlap R-34…R-63, which came from the internal code audit; the next
researcher continues from R-88.

### P0

### P1

### P2

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

### P3

- [ ] R-87 · P3 — Expose the fleet as a read-mostly MCP server
  Why: no tool in the survey unifies both vendors' session lists behind one interface, so an agent cannot ask what its siblings are doing — and the read half of that is cheap once R-72 lands.
  Evidence: prior art is spawn-only or single-vendor — agent-dispatch delegates to `claude -p` in other directories, claude-code-mcp is one-shot and single-agent, pal-mcp-server does cross-vendor subagent spawning; Kandev and Paseo expose their own platforms over MCP but not a unified cross-vendor session list. MCP spec 2026-07-28 removed protocol-level sessions and `Mcp-Session-Id` from Streamable HTTP, making the transport stateless and pushing state back to the server. Tool poisoning — malicious instructions in tool metadata, re-executed on every invocation — is the dominant 2026 MCP attack class per the NSA/CISA advisory and Microsoft's June 2026 warning.
  Touches: new crate or daemon subcommand, `crates/terminalai-daemon/`
  Acceptance: a stdio MCP server exposes read tools (list sessions, read status, read a session's last output, read fleet cost) with no gating; any mutating tool (spawn, kill, send) is opt-in per session, requires an out-of-band token, and logs every invocation into the diagnostics timeline; the server refuses to expose transcript content by default. Depends on R-72.
  Complexity: M
