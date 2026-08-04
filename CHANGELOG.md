# Changelog

All notable changes to TerminalAI are recorded here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [0.6.0] — 2026-08-03

### Added

- A comprehension pass over the fleet screen. The empty state now points at registering a project
  root rather than only offering to launch one session, and `?` in the toolbar opens a short
  explainer of the thing nobody guesses from looking at the screen: a row is not a terminal, which
  is exactly what lets thirty sessions share one. Its list of states is generated from the same
  table the rows are drawn from, so a status added later cannot appear on a row while missing from
  the explanation. Every status and the dwell timer carry a plain-language description, and a test
  now asserts that every icon-only control — in the markup and in the rows built at runtime —
  carries both a tooltip and an accessible name.

- A stored prompt library and a work queue that runs one prompt across many projects, creating a
  session per project as the fleet has room. Distinct from broadcast, which targets sessions that
  already exist; this one creates them, so it is far more careful. A repository with uncommitted
  changes is flagged rather than launched into — an agent let loose on a dirty tree mixes its work
  with the operator's, and the resulting diff cannot be separated afterwards — and a tree Git
  cannot read counts as not clean, never as clean. The check runs when the entry is about to
  start, not when the run was created, so a tree cleaned up in the meantime is not still flagged
  from an hour ago. Admission stays the fleet's decision: the queue asks for one slot at a time and
  stops when the answer is no. Every outcome category is reported, including the ones that did
  nothing, because a run over forty projects that says only "done" is one the operator has to audit
  by hand. Runs survive a restart.
- The prompt is delivered as a bracketed-paste pty write, not as a command-line argument: the
  session is launched with no initial prompt and the text goes onto its prompt queue. These
  prompts are kilobytes of prose, and a command line is the wrong place for that on any platform —
  an impossible one on Windows, where quoting mangles `&`, `^`, `|` and `%`.
- The library seeds itself once from the operator's own templates in `~/.claude/prompts` when they
  are present. Nothing is invented when they are not: a stored prompt named "drain the roadmap"
  containing something this app made up would be worse than an empty library. A seeded prompt the
  operator deletes stays deleted.

- A prompt queue per session. Queue what the agent should do next while it is still working, and
  each prompt is sent when the run finishes — a session becomes something you load up and leave
  rather than something you come back to. The queue advances on the same reported status the fleet
  row is drawn from, never on a timer: a timer would fire into the middle of a long tool call,
  where the prompt is ignored or read as an answer to something else. A run that ends waiting for
  a permission decision or asking a question pauses the queue instead of answering blind, and says
  which of the two it is. Only one prompt is in flight at a time — writing to the pty does not
  change a session's status, so without a hold between "sent" and "picked up" the whole queue
  would fire in one burst. Prompts are addressed by id rather than position, since the operator is
  the one reordering them, and can be edited or withdrawn until they fire; an action that raced a
  fired prompt is reported rather than silently doing nothing. Queues survive a daemon restart and
  come back paused, because a restored session is not running. `≡` on each row opens it, and
  `terminalai-probe queue <id> [add <text>|pause|resume]` drives it from the command line.

- A Projects view showing which known projects still have roadmap work: open items, how long ago
  the roadmap was touched, and the next unchecked item, sorted by most work first. Two states are
  kept distinct from zero throughout, because both would otherwise sort beside a finished project
  and drop out of consideration: a project with no `ROADMAP.md` is *unknown*, and a roadmap
  written as prose rather than checkboxes is *unreadable* — not empty. On the machine this was
  built against, 184 of 318 repositories fall into that second case, so treating it as zero would
  have reported 184 projects as finished. Checklist items inside fenced code blocks are skipped,
  so a roadmap documenting its own format does not count its examples as real work. Staleness
  comes from the file's modification time rather than a `git log` per project, which would be a
  process per project for a number that only needs to be approximate. Launching from a row carries
  that project's folder into the launcher.

- Register a root that holds your repositories — `~/repos` — and every Git repository under it
  becomes a launch target, so starting a session no longer means browsing to a folder you have
  visited a hundred times. The list is re-discovered on every launcher open rather than cached: a
  repository cloned five minutes ago is launchable without telling the app, and one deleted last
  week stops being offered. Discovery stops at a repository rather than descending into it, since
  otherwise every submodule and vendored dependency becomes a project of its own and a list of
  thirty becomes a list of four hundred; heavy directories are skipped and depth is bounded, so a
  root registered by mistake cannot turn into a full tree walk. Registering reports how many
  projects it found, because "registered" alone cannot distinguish a working root from one pointed
  at the wrong directory. A root already covered by another is refused, and a broader one replaces
  the roots it covers. Measured against a real 318-repository tree.

- The launcher ships with built-in presets, so a fresh install offers something to pick instead of
  an empty dropdown that asks the operator to invent a configuration before they know which ones
  matter. Six span the axes actually decided between — which agent, and how much rope it gets —
  each with a one-line description rather than four words of jargon. The preset that never asks
  permission is also the one that runs in its own worktree; shipping the dangerous half of that
  pair without the safe half would be the app recommending it. Built-ins are marked in the
  dropdown, cannot be edited in place — an edited copy would silently outlive a corrected version
  in a later release, so saving under a built-in's name is refused with the suggestion to clone —
  and are hidden rather than deleted, because a preset that exists only in code cannot be
  recreated by hand. `↺` beside the preset list offers every hidden built-in again.
- Presets saved before this version still load: the store read a bare JSON array and now reads
  either shape, rewriting in the current one on the next save.

### Fixed

- Environment leases now reject escaping compose files, canonicalize accepted paths inside the
  repository before teardown, and ignore repository attempts to enable destructive volume removal.
- Work runs now mark exited or removed sessions as done and automatically admit
  the next pending project when a fleet slot opens.
- Transcript tailing now advances past oversized and invalid-UTF-8 JSONL records instead of
  retrying the same offset forever. The bounded byte reader preserves partial-line behavior while
  skipping records the JSON parser cannot safely consume.
- Release `terminalai.exe` now uses the Windows GUI subsystem, so Explorer and Start-Menu launches
  do not allocate a companion console. The installer gate reads the installed PE header and fails
  unless it reports subsystem 2.
- The Preflight and Review views now open correctly. Their visibility synchronizers reference the
  column-label element by an id that exists in the shell, with a DOM regression test covering every
  element named by both synchronizers.
- All 55 Tauri commands now have grants in every desktop capability profile. The build checks both
  the `generate_handler!` registration and the capability files, so a newly registered command
  cannot compile into an unusable invoke path or leave a stale permission behind.
- Applying a preset that names no working directory no longer blanks the folder. Which
  configuration to use and which project to run it on are separate choices.

## [0.5.0] — 2026-08-03

### Added

- Repositories can declare their own launch templates in `.terminalai/templates.toml`, versioned
  with the code they describe, so starting work on a familiar project does not begin by
  remembering which permission mode and effort level it wants. Templates appear in the launcher
  when that folder is chosen and are re-read on every change, because pulling a branch that edits
  the file should change what the launcher offers. A template may only set choices the launcher
  already models: `extra_args` and `cwd` are refused at the schema, since the file arrives with a
  clone and one that could put arbitrary text on an agent's command line would be argument
  injection; extra writable directories are refused if they escape the repository; and an
  unrecognized value for agent, effort, permission or sandbox is a refusal rather than a string
  passed through to the CLI. A malformed file is reported, never treated as no templates. This
  repository ships its own, which is also what the format's test reads.

- Broadcast one prompt to several sessions, from a dialog in the fleet toolbar or with
  `terminalai-probe broadcast <id>... -- <text>`. The result is reported per session, never as one
  status: a broadcast that says only "sent" leaves the operator unable to tell which agents got the
  prompt, and re-sending to find out delivers it twice to the ones that already had it. A session
  waiting on a permission decision is refused by name — a permission prompt is a specific question
  with a small set of valid answers, and prompt text answers something, just not what was meant —
  while a session merely asking a question does receive it. Ineligible sessions are listed in the
  dialog with their reason rather than hidden, the selection is re-checked at send time because a
  session can enter a permission prompt while the operator is typing, and the CLI exits non-zero if
  anything was refused.

- Cost and token rollup across the fleet. The header's spend figure is now a control that opens a
  breakdown by agent, by project folder, and by session, each with its own token totals — cost and
  tokens answer different questions, and a run heavy in cache reads costs about what one heavy in
  output does while behaving nothing alike. `Session.tokens` carries the transcript's totals
  alongside the price they produced. Every grouping states its own coverage: a session whose
  transcript has not been read is counted apart and shown as an em dash, never folded in as zero,
  because zero is the claim that a session ran and spent nothing — and treating "unknown" as zero
  makes the total quietly too low exactly when someone is checking whether it is too high. A cost
  under a cent reads `<$0.01` rather than rounding to `$0.00`, which would be indistinguishable
  from a session that has not started spending.

- A private Git worktree per session, requested from the launcher and cleaned up with the row.
  Two agents on one repository was the fleet's most obvious use and its worst failure: one
  session's uncommitted edit became another's unexplained diff. Each isolated session gets its own
  checkout on its own `terminalai/<id>` branch, cut outside the repository so it is not untracked
  clutter in the parent's status. Nothing is ever reused — an existing branch or directory is
  refused rather than adopted, because neither this code nor the operator can tell leftovers from
  unlanded work. Removal takes the checkout but offers the branch to `git branch -d`, so a branch
  holding unmerged commits is kept and reported instead of deleted; a checkout deleted behind
  Git's back is repaired by pruning rather than left as a registration that breaks every later
  add. Repository leases now copy their untracked config from the repository the checkout was cut
  from, which is what makes an isolated session runnable at all. `--worktree` on
  `terminalai-probe start`.

- Scrollback to disk, under the bounded in-memory ring. Each session appends to a rotating pair of
  segments capped at 8 MB total, so history outlives the 512 KB the ring can hold, and survives a
  daemon restart. The bound is in bytes rather than lines because a line costs whatever the pane is
  wide — the same "10,000 lines" is three times the storage at 360 columns as at 120. Writes are
  queued to a dedicated thread: output arrives on the pty reader with the registry's state lock
  held, and a blocking write there would stall every other session and back-pressure the agent that
  produced the bytes. If that queue ever fills, the bytes it drops are announced in place in the
  log rather than silently omitted. `⇡` in the terminal toolbar loads older output into the focused
  pane, and `terminalai-probe history <id> [bytes]` reads it from the CLI.

### Changed

- The session store no longer carries session output. The log is now the durable copy, so the
  store's debounced rewrite stopped copying every session's whole ring — up to 30 × 512 KB as often
  as once a second — and a restarted daemon replays its panes from the log instead.

### Fixed

- A session no longer adopts the transcript of another session running in the same folder.
  Discovery ranked candidates by modification time, so a run that was already in progress — and
  therefore being appended to right now — always looked newer than the file the new session was
  waiting for, and its cost, token totals and resume id were reported against the new row.
  Candidates are now ranked and floored by creation time, the only stamp that says which run a file
  belongs to, with a 100 ms grace below the floor for the coarse system clock that stamps files.
  Ties are broken by path so the same directory always yields the same answer.

## [0.4.0] — 2026-08-03

### Added

- Filter and group the fleet list. Agent and status dropdowns sit alongside the free-text box —
  text matches anything on a row, while these are exact dimensions an operator thinks in. A group
  button cycles none → folder → agent → status and always states the mode it is in. Grouping
  reorders so a group's members are adjacent and labels each row, rather than inserting headers:
  the list is an ARIA listbox, and a child that is not an option would break its semantics and its
  keyboard model. A group's position follows its most urgent member, so the folder holding a
  session that needs you stays at the top.
- Windows toasts for sessions that want the operator. A session entering `NeedsYou`,
  `NeedsApproval` or `AwaitingInput` raises a toast naming it, why it is blocked, and which repo it
  is in — a toast reading only "Needs you" is useless with thirty sessions. Clicking it focuses
  that session and raises the window, using the in-process `on_activated` handler rather than a COM
  activator: the app is by definition already running when it raises the toast. A rate-limited
  session deliberately does not toast — it is blocked, but there is nothing the operator can do.
  Toasts are short-duration because an unpackaged process cannot reliably withdraw one from the
  action centre, and a toast outliving the state it describes sends the operator to a session that
  has moved on. A missing Start Menu shortcut makes every toast silently fail, so that is reported
  once rather than per event.
- Split view for pinned sessions. Up to three pinned sessions now render live panes beneath the
  focused terminal, drawn from the Rust-side grids the daemon already kept for them rather than
  from more xterm instances — one renderer is what lets the fleet hold ~29 rows, and three more
  would undo that. Panes are reconciled by session id so one snapshot cannot blank a sibling, a
  pane with no snapshot yet says so rather than showing an empty box, and unpinning drops the
  stored grid so repinning cannot show a stale frame. `terminalai-probe pin` and
  `terminalai-probe grid` drive the same path from the command line.
- Transcript tailing. Each live session's JSONL is followed incrementally — only the bytes appended
  since the last poll — for the three things the pty cannot carry: the agent's own session id (what
  `--resume` takes), the last thing it actually said, and what the run cost. `Session.cost_usd` and
  the fleet's aggregate are now real numbers instead of a computed-looking zero; measured live at
  $0.98 for one session against its own transcript. A half-written record is left for the next poll,
  a truncated file restarts rather than splicing two records together, and a deleted one is
  rediscovered. Only `text` blocks become the row label, so a tool call's arguments never do.
- `Session.last_message` carries the transcript's text and the row prefers it over `last_line`,
  which is the tail of a rendered TUI and carries whatever escape sequences the last redraw left.

- `terminalai-probe mcp` exposes the fleet to an MCP client over stdio. Read tools — list sessions
  (both supervised and external), read one session's status, read the tail of its terminal output,
  read fleet cost — are ungated. Mutating tools require *both* an out-of-band write token and a
  session opted in when the server starts, are not advertised at all unless enabled, and every
  attempt is logged whether it was allowed or refused. Session transcripts are never exposed and
  the session summary is a whitelist, so a field added to `Session` later cannot leak through a
  tool that was reviewed once. Tool metadata is compile-time constant — asserted by comparing the
  advertised tools across two different fleets — because tool poisoning is only possible when
  metadata comes from somewhere mutable. Terminal output is stripped of escape sequences before it
  leaves the process.

### Fixed

- A session launched into a folder that already held transcripts bound to the newest existing one
  on its first poll, reporting an earlier run's cost, token totals and resume id as its own —
  observed live before the fix. Discovery now ignores transcripts older than the session.
- Records without a `requestId` were summed. Codex reports the session's *cumulative* usage on
  every turn, so summing multiplied the real figure by the number of turns; a cumulative record now
  replaces rather than accumulates, and never walks backwards.

## [0.3.0] — 2026-08-03

### Added

- Repositories can declare a per-session environment lease in `.terminalai/environment.toml`, so it
  is versioned with the code it describes. Beyond the deterministic port block that already existed,
  a lease covers the three things parallel sessions actually collide on: untracked config copied in
  by glob, a docker compose project prefix so two sessions build two stacks, and a Postgres database
  cloned per session with `CREATE DATABASE … TEMPLATE …`. Leases are released when the session tears
  down, and the raw setup/teardown script escape hatch is unchanged. Depth over generality is
  deliberate — a generic hook API is what every other tool already tells the operator to write.
  A lease that cannot be read, escapes the repository, or names a database that is not a plain
  identifier refuses the launch rather than being ignored, because a session that quietly falls back
  to the shared database is the exact collision the lease exists to prevent.
- A session's work can now be landed from the review surface, through a gate that refuses rather
  than half-applies. Landings are serialised daemon-wide, so a request that waited in the queue is
  checked against a fresh read of the target rather than against what the review showed. A landing
  is refused whole — naming the one specific condition — when the target moved since review, the
  target tree is dirty, conflict markers are present, the patch no longer applies, or a configured
  verify command fails; a failed verify reverses the patch it just applied, and the one case where
  that reversal itself fails is reported as a mixed state needing manual repair rather than folded
  into a generic error. Nothing is merged, staged, committed, or auto-resolved on the operator's
  behalf, and the landed change is left uncommitted because committing is their decision.
  `terminalai-probe land` drives the same path from the command line.
- `ReviewItem` now carries `target_head`, the commit a review was read against. The moved-target
  refusal compares against it; without it that check would have had nothing to compare.
- Rate limiting is now a first-class row state. A session a provider is refusing renders as
  `Rate limited` with which quota tripped and when it reopens, sorts with the attention states
  rather than with the busy ones, and releases its admission slot so a queued session can take it.
  The fleet header counts limited sessions and shows the soonest reset across the fleet. The state
  is only ever entered from an explicit agent report — Codex's `rate_limits` table (the
  most-consumed window wins) or a Claude retry carrying a `rate_limit`/`overloaded` category — never
  from a session going quiet, which is indistinguishable from a long tool call. A missing reset time
  is said out loud rather than guessed, and a plain transport error is not treated as a quota.
  Verified end to end against a running daemon: a `weekly` window at 100% won over `primary` at 31%,
  live sessions went 1 → 0, and a later report with room left returned the row to the fleet.
- The focused pane now runs the WebGL renderer. xterm 6.0 removed `addon-canvas`, so with no WebGL
  addon the DOM renderer — the slowest of the three — was the only one available. Context creation,
  addon construction, and later context loss each fall back to the DOM renderer rather than blanking
  the pane. Measured in a Chromium engine: 2 canvases attached, renderer reports loaded.
- OSC 8 hyperlinks emitted by a session are now clickable. They already reached the pane, because
  the focused renderer replays raw PTY bytes, but without a link handler xterm underlined them and
  clicking did nothing. The URI is agent-controlled, so it is opened only after Rust accepts the
  scheme: `http`, `https` and `mailto` only, control characters refused, and every refusal reported.


- Added Windows process-hygiene controls: background ConPTY sessions use reversible EcoQoS and
  memory-priority settings, focus and pin changes restore normal priority, waiting-session counts
  render as a numeric taskbar overlay, and `terminalai-probe hygiene` publishes repeatable console
  churn and input-latency measurements.
- Added one shared Fluent catalog at `web/src/i18n/terminalai.ftl`, loaded and validated by the
  Rust daemon and formatted by the web renderer. Status diagnostics now carry structured reason
  kinds with arguments, while localized counts and dwell labels use `Intl.PluralRules` and
  `Intl.RelativeTimeFormat` without expanding the compact fleet row.
- Expanded Claude/Codex hook coverage across prompt, tool failure, permission, subagent,
  compaction and session-end events. Claude uses a daemon-lifetime bearer-authenticated loopback
  HTTP endpoint with an exact Host allowlist and Origin rejection; installation falls back to the
  existing fail-open command adapter when HTTP cannot be configured. Unknown event names remain
  visible in diagnostics instead of being discarded.
- Replaced static launcher model and effort lists with cached runtime capability probes. Codex uses
  `model/list` plus `codex features list`; Claude reads `system/init`. Per-model effort order is
  preserved, values are invalidated when the resolved binary version changes, and unknown user
  values remain launchable with warnings.
- `SessionRegistry` now supervises sessions through an object-safe `AgentDomain`/`AgentSession`
  contract. `LocalPtyDomain` keeps the existing ConPTY behavior as the default while a future
  remote relay can supply input, output, resize and lifecycle operations without local handles.
- Session persistence now carries a `TerminalAI.session-store` magic string and schema 2. Legacy
  schema 0 and 1 files migrate atomically after a `sessions.v<old>.bak` copy, future versions are
  still refused, and unknown top-level fields survive through `serde(flatten)` and registry writes.
- The contrast audit now keeps small text on WCAG-AA tokens in both dark and OS light palettes,
  uses `surface0` for selected rows, and treats overlay/surface tokens as decoration only. The
  focused terminal has an opt-in xterm screen-reader toggle with its right-click behavior called
  out in the control label.
- Forced-colors mode maps fleet status, controls and the xterm surface to system colors while
  keeping `forced-color-adjust: none` limited to xterm. Reduced-motion mode also stops the fleet
  spinner and status glow effects.
- Added structured daemon and desktop-shell logging under `%LOCALAPPDATA%\TerminalAI\logs\`
  with fourteen daily files, a bounded in-app log panel, per-session tracing fields, and panic
  records that drain through the process-held worker guard.
- The Windows pipe ACL now grants the current interactive user's explicit SID
  (plus `SYSTEM`) instead of the elevation-sensitive owner-rights alias. PID
  mismatches are reported as diagnostics, and setup/teardown hooks are clearly
  documented as opt-in local shell execution.
- The daemon now has a graceful `terminalai-probe shutdown` control request, Windows console
  teardown handling, and a single-instance binding guard. Protocol skew reports the running daemon
  PID and a concrete stop command; the desktop shell refuses to spawn a second daemon in that case.
- The control endpoint now has a stable name with a legacy v2 fallback, so protocol negotiation can
  detect upgrades without stranding a daemon that still owns live sessions.

### Changed

- xterm now measures character widths with the unicode11 addon. It defaulted to Unicode 6 (confirmed
  by reading `unicode.activeVersion` in a real browser) while the Rust grid uses `unicode-width`
  against a modern table, so the two disagreed about where a line wraps and the status inferred from
  the Rust grid could stop describing what the pane showed.

- Agent resolution now runs against an injectable filesystem (`which` 8's `Sys` trait), so the npm
  prefix and `PATH` routes are covered by tests instead of by whatever happens to be installed on
  the machine running the suite. Eight cases are now exercised directly, including an unpopulated
  Windows `PATHEXT` — harmless only because the query carries an explicit `.exe`, which is now
  asserted rather than assumed. Non-fatal search errors are logged instead of collapsing into an
  indistinguishable "not installed".
- Updated `toml_edit` 0.20.2 → 0.25 (`Document` → `DocumentMut`). Release binaries shrank: daemon
  2,214,912 → 2,192,896 bytes, probe 1,693,184 → 1,643,008 bytes. Note the lockfile still carries
  three `toml_edit` copies, but only one was ever compiled into a Windows binary — the other two
  reach the lockfile through `glib-macros` (Linux GTK) and `num_enum_derive` (Android) and are
  excluded by target. The direct dependency now shares the 0.25 copy rather than adding a fourth.

### Fixed

- Environment teardown failures during a failed launch were discarded with `let _`, so a leaked
  database or a compose stack left running was indistinguishable from a clean unwind. They are now
  logged with the session and the cause. Teardown also reports *every* failure rather than the
  first, since a container left running matters even when the database also failed to drop.
- A session that opened a synchronized update (DEC 2026) and then stopped writing — killed
  mid-frame, or with its write truncated — froze that session's terminal grid permanently. `vte`
  buffers everything between `ESC[?2026h` and `ESC[?2026l`, and arms a 150ms deadline when the
  update opens, but reports only that a deadline *exists*, never that it expired, so the buffer was
  never flushed and every status inferred from the grid went quietly stale. Expiry is the caller's
  job and now happens on both the write and the read side.

## [0.2.0] — 2026-08-03

### Added

- `terminalai-core::registry` — Rust-owned session lifetime, sorted fleet snapshots, pushed
  output/status events, focus/read/pin controls, and bounded per-session scrollback.
- `terminalai-app` — Tauri 2.11.5/WebView2 desktop shell with a Catppuccin Mocha fleet list,
  28px status rows, launcher dialog, exact command preview, folder pickers and saved presets.
- One canonical 120×40 xterm renderer that replays focused-session scrollback and only resizes on
  the explicit terminal reset action.
- Normalized Claude/Codex hook ingress through the authenticated daemon pipe, with non-blocking
  `terminalai-probe hook` delivery and separate approval/input attention states.
- `terminalai_core::transcript::TranscriptAccumulator` — nested usage extraction, requestId
  deduplication, and cost calculation through a caller-pinned pricing-table version.
- Explicit session supervision state now separates agent phase from process health, exposes PID,
  resume id, exit metadata and restart counters, and retries unexpected exits with exponential
  backoff before entering a terminal failed state.
- Review aggregation now uses a bounded worker pool with per-repository Git deadlines, capped
  incremental output capture, process-tree cleanup on Windows timeouts and an explicit timed-out
  partial-result state.
- Reversible agent hook configuration now previews, installs, reports and removes only
  `--terminalai-managed` Claude JSON and Codex TOML entries; Claude handlers are asynchronous,
  Codex preserves unrelated `notify` commands, and the installed app hook path fails open when the
  daemon is unavailable.
- The fleet list now exposes distinct glyphs and visible labels for every status, keyboard row
  activation, screen-reader announcements, light/dark contrast tokens, forced-colors support and
  reduced-motion behavior.
- Fleet navigation now exposes a single-select listbox with roving keyboard focus, option position
  metadata, explicit selection state and real per-row action buttons.
- Fleet status announcements now coalesce actionable transitions for two seconds, while sorting
  pauses during list interaction and exposes an Apply control for pending priority moves.
- The desktop shell now opens even when the daemon is unavailable and presents a first-run
  preflight panel for agent versions, managed hooks, daemon reachability and the Start-Menu app ID,
  with local Fix and Recheck actions that remain reachable from Diagnostics.
- Release checks now use cargo-deny against the Windows target, denying unreviewed vulnerabilities
  and documenting the five currently unavoidable transitive Unicode advisory exceptions with
  upstream RustSec links.
- `terminalai-core::grid::TerminalGrid` now parses each session's ANSI output into a bounded Rust
  grid with cursor motion, scrolling, alternate-screen restore and split UTF-8 coverage. Only the
  focused session (and future pinned panes) receives live output events; background sessions keep
  parsed state without creating more xterm instances.
- R-14 restore actions now reattach live rows with bounded replay, revive stopped rows through
  Claude/Codex native resume ids, or archive layout/cwd/command metadata. A debounced daemon worker
  persists the versioned JSON store without automatically restarting recovered agents.
- Corrupt, truncated, and unsupported-version session stores are quarantined with a timestamped
  filename so the daemon starts with an empty fleet and the desktop shell names the saved path in a
  dismissible banner.
- Fleet ordering now compares persisted status-transition timestamps and stable session ids, so a
  snapshot cannot become non-transitive while it is being sorted.
- Fleet rows now reconcile by session id, mutating existing row content while preserving focused
  reply inputs and their caret across live updates and filtered views; frontend coverage runs with
  `npm --prefix web test`.
- PTY output and replay now stay as raw bytes from the registry through the daemon and Tauri
  `Raw` channel, with 12 ms batching so split UTF-8 sequences reach xterm intact.
- Windows PTY sessions now belong to kill-on-close job objects, so manual kills and session drops
  reap descendants; daemon teardown runs each active session hook once and releases its port block.
- The PTY environment now preserves the Windows user/runtime directories, processor count and
  present HTTP(S)/no-proxy settings; installed Claude and Codex binaries are covered by sanitized
  `--version` launch tests while parent-only secrets remain excluded.
- Session persistence now wakes a worker with a lightweight signal; it builds the registry snapshot
  after the debounce and stores replay tails as per-session binary sidecars instead of cloning and
  JSON-encoding a full ring for every output event.
- Persistence now writes after a quiet period or a one-second maximum interval, even under
  continuous output, and always snapshots the latest registry state.
- Row summaries now scan a bounded tail of each ring buffer instead of copying and decoding the
  full 512 KiB scrollback for every output chunk.
- Background grids now honor DECSTBM scroll margins, saved-line erase, terminal resize, custom tab
  stops, automatic wrapping, and Unicode wide-character cell widths, with recorded CLI reference
  streams and arbitrary-byte parser coverage.
- A poisoned registry lock is now recovered safely; the daemon keeps serving health and metadata
  requests while returning an explicit state-unavailable error for fleet mutations and snapshots.
- Notification retraction is now session-wide across attention-state changes and archive/kill/remove
  paths, preventing stale badges and toasts.
- Automatic restarts now share one bounded timer scheduler; admission-blocked attempts consume
  restart backoff and budget, and scheduler failure marks the session failed instead of abandoning it.
- Environment setup and teardown hooks now run on dedicated workers with visible preparing and
  teardown phases; Windows hook timeouts terminate the entire descendant process tree.
- Registry and IPC event delivery now use bounded nonblocking queues with counted drops, and each
  subscribed connection stops and joins its event bridge before the writer thread is joined.
- R-16 adds lifecycle-aware attention notifications with stable dedup keys, repository grouping,
  automatic retraction on progress, and startup/long-tool grace suppression. The shell consumes
  raised notifications as deduplicated, click-to-focus in-app alerts.
- R-17 adds daemon-owned admission control with a configurable live-process cap, FIFO overflow
  queue, a Claude default spend cap, and a live/queued/aggregate-spend fleet summary.
- R-18 now treats failed process reconciliation as an explicit unknown state, retaining the row and
  PTY until a later positive exit result proves that the process is gone.
- R-19 adds an opt-in `codex-app-server` daemon transport. The core accepts Codex JSONL/JSON-RPC
  notifications, preserves unknown methods, maps status and approval signals into the fleet event
  model, and encodes steer/interrupt requests while the default daemon remains unchanged.
- R-20 promotes the headless probe into a daemon control client with JSON `list`, `start`, `stop`,
  `send`, and `status` commands. It uses the same named-pipe protocol as the GUI, returns one stable
  JSON object per invocation, and keeps connection/remote failures on a nonzero exit path.
- R-21 reshapes fleet rows around status glyphs, repository/branch, dwell, tool-progress fractions,
  restart counts and ellipsized output, with an all-state header strip, `/` filtering, and a Wide
  view for model, effort and reported cost.
- R-22 adds deterministic per-session service-port blocks, optional setup/teardown shell hooks,
  hook/agent environment variables, launcher and probe controls, and allocated-port row metadata.
- R-33 adds a daemon-owned Review view that aggregates Git diffs across sessions, ranks review cost,
  preserves conflict markers, and lets the operator mark a session reviewed.
- R-25 adds real-pty echo coverage for output and exit detection plus version-pinned Claude Code
  and Codex CLI launch-argument golden fixtures.
- R-26 adds bounded per-session status evidence, a focused "why this state" diagnostics view, and
  structured JSONL crash records for daemon or app panics.
- R-28 adds unsigned NSIS/MSI packaging with Evergreen WebView2 bootstrapper mode and a read-only
  GitHub release check in the shell; update checks never download or install an artifact.
- R-34 routes hook settings, session state and launcher presets through one fsync-backed atomic
  writer with advisory locking and `.bak` recovery copies.
- R-35 terminates launch options before positional prompts, rejects non-finite or negative Claude
  budgets, and documents the trusted-input-only extra-argument escape hatch.
- Claude/Codex hook payloads now share one parser, including session/thread id aliases and
  permission/idle notification normalization for approval and awaiting-input states.
- The fleet now has a needs-input filter and per-row reply controls that send bracketed paste
  through the daemon without switching terminal focus.
- Strict agent/binary matching before a process is spawned, preventing a launcher configuration
  from accidentally starting the wrong CLI.

### Fixed

- The core now passes strict clippy; `Agent` uses a derived default and fleet tests use arrays
  where allocation is unnecessary.
- PTY children now receive an explicit safe environment allowlist instead of inherited API keys,
  registry secrets and unrelated agent credentials.
- Poisoned PTY locks return `PtyError::Gone` rather than panicking the host process.
- Codex now emits its supported `max` reasoning effort and explicit `Plan` collaboration-mode
  config override.
- Documentation now distinguishes thirty tracked sessions from the much smaller live-process
  budget measured on the development machine.
- `terminalai-daemon` now owns the registry behind a versioned local-socket protocol with an
  explicit event subscription, peer-PID handshake, owner/SYSTEM Windows DACL and no client
  impersonation path.
- Resume variants now use an explicit JSON `id` field, and the unsigned NSIS/MSI release lane
  builds the Vite frontend and app bundle from the workspace root.
- The focused terminal is now visible: the placeholder is hidden the moment a session attaches
  and restored when focus clears, so the xterm renderer occupies the host box instead of being
  laid out below it.
- Closing the launcher, or pressing Enter in any launcher field, no longer spawns an agent —
  every dialog control is an explicit button and the form refuses submission outright.
- The focused terminal now fits its pane. The fit addon was constructed and registered but never
  called and there was no resize listener, so the grid stayed at a hard-coded 120x40 regardless of
  pane size. Measured in WebView2's engine: 99 columns at a 1356px window, 141 at 1920px, and back
  down when the panel narrows. Resizes are debounced by 180 ms and skipped when the size is
  unchanged, because agent TUIs hard-wrap and a resize arriving mid-drag corrupts the output the
  supervisor parses for status.
- A focus switch now carries a generation token, so output that arrives late for the session you
  just left is discarded instead of being written into the new session's grid.
- The control protocol enforces a 1 MiB frame limit on read, so a peer that sends bytes without a
  newline can no longer exhaust memory on either end. Oversized `Write` payloads are refused whole
  with a typed error rather than truncated — half a prompt reaching an agent is worse than none.
- A malformed control frame now returns an error and keeps the connection, instead of tearing it
  down with no reply, and a transient thread-spawn failure in the accept loop drops that one
  connection instead of ending `serve()` and abandoning every live agent.
- The daemon restricts library loading to System32 before any pseudo-console is created.
  `portable-pty` asks for `conpty.dll` by bare name and no such file exists in System32 on this
  OS build, so the search reached the application and working directories — both writable by a
  non-administrator — in the process that owns every supervised agent.
- The app's own commands are now covered by declared Tauri ACL policy rather than only a runtime
  patch: `build.rs` declares an `AppManifest` listing all 26 commands, and the capability drops
  from the nine sets `core:default` expands to down to the four actually used, marked local-only.
  A build assertion fails, naming the command, if one is added without a matching grant — the ACL
  is checked at invoke time, so that failure would otherwise reach a user instead of the build.
- The CSP now names `base-uri`, `form-action`, `frame-ancestors` and `object-src` explicitly.
  None of them inherit from `default-src`, so the previous policy left an injected `<base>` or
  `<form>` unconstrained.
- Every interpolation into a markup attribute goes through `escapeHtml`, and a test enforces it —
  agent output reaches those strings, and one unescaped attribute is enough to break out of it.
  Agent output also still cannot reach the system clipboard: the xterm OSC 52 clipboard addon is
  deliberately not a dependency, now asserted rather than assumed.
- Sessions started outside TerminalAI now appear in an **Elsewhere on this machine** panel, read
  from Claude Code's per-PID session registry with `claude agents --json` as the reconciliation
  fallback. The rows are read-only and carry no controls, because the supervisor owns none of
  those processes. Identity is `(pid, procStart)` so a reused PID cannot inherit another
  session's row, and an unreadable registry reports "unknown" rather than an empty machine.
- The cost model can now express the rates transcripts actually carry: separate 5-minute and
  1-hour cache-write tiers, the regional-inference premium and the priority-speed premium. The
  previous four-field type could represent none of them, so the first spend figure the fleet
  reported would have under-stated by up to half on a cache-heavy turn.
- A commit-pinned LiteLLM price snapshot is vendored at
  `crates/terminalai-core/pricing/model-prices.json` and embedded in the binary — never fetched at
  runtime — with a hardcoded fallback and a version string the fleet header names in its tooltip.
  Dated and region-prefixed model aliases resolve to their base model.
- Transcript usage parsing no longer resolves nested `usage` objects alphabetically (the JSON map
  is a `BTreeMap`, so a sibling key could win over `message`), and the deduplicated request-id set
  is bounded instead of growing for the life of the daemon.
- `branch` is now real: read from the session directory's Git HEAD at launch and refreshed from
  hook events, rate limited to one lookup per session per 30 seconds so a per-tool-call hook does
  not become a process per tool call. A directory outside a repository, a detached HEAD, or a Git
  that does not answer within 1.5 seconds all render an em dash instead of a guess.
- `tool_progress` is now populated from the agent's own plan — Claude Code's `TodoWrite` and
  Codex's `update_plan` both carry a countable list. A tool call with no plan leaves the previous
  value alone, and a session that has never reported one renders an em dash rather than `0/0`.
- A session that has never reported a cost renders an em dash instead of `$0.00`, in the row and
  in the fleet header. `Number(null)` is `0`, so the header was reporting a computed-looking zero
  for a figure nothing writes yet.
- The compact fleet row now meets its documented 28px, down from 54px, putting the status glyph,
  name, repository, agent, tool progress, restart count, dwell and last output line on one line
  and moving branch, ports and the spelled-out status into Wide. Measured in WebView2's engine at
  the default 1440x900 window: 29 fully visible rows against 13 before.
- Blocks carrying the `hidden` attribute are hidden again. An author `display: grid`/`flex`
  declaration outranks the user-agent `[hidden]` rule, so the Wide metadata and inline reply
  blocks were rendering on every row regardless.
- A session marked reviewed now returns to unreviewed as soon as its diff changes. The mark
  records a fingerprint of the diff it was made against instead of a write-once boolean, so a
  session no longer stays acknowledged while the agent keeps editing files. Marking a session
  whose working tree cannot be read is refused rather than recorded.
- Session exit detection now blocks on the child's process handle instead of a per-session thread
  waking twenty times a second to ask whether it had finished. Environment setup and teardown
  hooks use one bounded wait rather than a 25 ms poll. Measured over 60 seconds with ten idle
  sessions: 218.8 ms of CPU per minute before, below one scheduler tick after. Polling remains
  only where no waitable handle exists.
- `terminalai-probe cpu-idle` measures that supervision cost on demand, with `--poll` to reproduce
  the old strategy for comparison on the same machine.
- `panic = "abort"` is gone from the release profile. One panic on any daemon worker thread used
  to terminate every supervised session at once, and the poisoned-lock recovery arms were dead
  code in the shipped binary while the test profile forced unwinding. A build-time guard in
  `terminalai-daemon` refuses to compile under abort. Release binaries grow about 0.5 MB.
- The declared MSRV is now 1.88, the real floor set by resolved dependencies. The workspace
  claimed 1.82, which cannot build the tree — the README documents the `cargo metadata` command
  that re-derives the floor so the next dependency bump is caught rather than discovered.
- The NSIS and MSI bundles now ship `terminalai-daemon.exe` and `terminalai-probe.exe` as Tauri
  sidecars, so an installed copy can start. Previously the app resolved a sibling daemon the
  bundle never placed next to it, and every installed copy exited before drawing a window.
- New release gate `scripts/verify-installer.ps1` installs the bundle into a scratch prefix,
  asserts every declared sidecar arrived, launches the installed binary on the isolated display
  with placement proof, waits for the daemon control pipe, then uninstalls.
- An operator-configured agent path is now positively identified before it is spawned: its file
  stem must match the agent and a cached, deadline-bounded `--version` probe must name it.
  Previously the downstream agent/binary guard compared two values that were equal by
  construction, so it could never fire on the configured-path route.

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
