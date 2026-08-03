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
- [ ] Status inference fallback for sessions started outside TerminalAI
      — *2026-08-02 research: when no hook is installed for an agent, render a visible degraded state. Never
      report idle from absence of signal — that is the exact failure mode in ccmanager#227 and cmux#1027.*
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

## Research-Driven Additions

From `RESEARCH.md`. IDs R-01…R-63; the next researcher continues from R-64.

### P0

- [ ] R-34 · P0 — Atomic, backed-up writes for every file the app does not own
  Why: `write_if_changed` truncates and rewrites the user's real `~/.claude/settings.json` in place, so a crash mid-write destroys their permissions, MCP servers and hooks — damage outside the app, to a file Claude Code also writes concurrently.
  Evidence: `crates/terminalai-core/src/hook_config.rs:542`; same class at `store.rs:102` and `crates/terminalai-app/src/preset.rs:94-97` (which deletes before renaming, though `fs::rename` on Windows already replaces atomically). Release profile is `panic = "abort"` (`Cargo.toml:37`), so any panic aborts mid-write.
  Touches: `hook_config.rs`, `store.rs`, `preset.rs`
  Acceptance: one shared helper does temp-file → fsync → atomic rename, keeps a `.bak` of previous contents for user-owned files, and holds an advisory lock for the duration; a test injects a write failure and asserts the original file is intact; no `remove_file` before `rename` anywhere.
  Complexity: M

- [ ] R-35 · P0 — Terminate the option list before user text reaches an agent CLI
  Why: a prompt beginning with `-` is parsed by the agent as flags, defeating the project's "refuse, do not drop" guarantee.
  Evidence: `crates/terminalai-core/src/launch.rs:249,322`. Verified 2026-08-02: `terminalai-probe preview claude --prompt "--dangerously-skip-permissions"` emits that string as a flag. Reachable: `--dangerously-skip-permissions`, `--permission-mode bypassPermissions`, `--sandbox danger-full-access`, `--add-dir C:\`.
  Touches: `crates/terminalai-core/src/launch.rs`, `tests/fixtures/launch/`
  Acceptance: `--` (or the agent's documented equivalent) precedes the positional prompt for both agents; `max_budget_usd` rejects NaN, negatives and infinities; golden fixtures pin a dash-leading prompt per agent; `extra_args` is documented as trusted-input-only and unreachable from a pasted prompt.
  Complexity: S

- [ ] R-36 · P0 — A corrupt or future-versioned store must never prevent daemon start
  Why: one bad byte makes `DaemonServer::bind` fail before the listener exists; the daemon is spawned with `CREATE_NO_WINDOW`, so the user sees only a generic 5-second timeout and must find and delete a file they were never told about.
  Evidence: `store.rs:81-86,90-104`; `crates/terminalai-daemon/src/lib.rs:221-227`; `crates/terminalai-daemon/src/main.rs:2-5`; `crates/terminalai-app/src/main.rs:282`.
  Touches: `store.rs`, `terminalai-daemon/src/persistence.rs`, `lib.rs`, fleet header
  Acceptance: an unreadable, truncated or unknown-newer store is renamed to `sessions.corrupt-<RFC3339>.json`, the daemon starts empty, and the UI shows a dismissible banner naming the quarantined path; fixtures cover truncated JSON, `schema_version: 0` and `schema_version: 999`.
  Complexity: M

- [ ] R-37 · P0 — Make fleet ordering deterministic
  Why: the comparator reads the clock, so it is not a stable total order; Rust ≥1.81 `sort_by` panics on detection and `panic = "abort"` turns that into a daemon kill on a path every session list hits.
  Evidence: `crates/terminalai-core/src/session.rs:443` calls `in_status_for()` → `SystemTime::now()` per comparison; `registry.snapshot()` is on every IPC list path.
  Touches: `crates/terminalai-core/src/session.rs`, `registry.rs`
  Acceptance: `fleet_order` takes an explicit `now: SystemTime` or compares `status_since` directly; a test sorts sessions created in the same tick and asserts no panic and a stable result across repeated sorts.
  Complexity: S

- [ ] R-38 · P0 — Stop the fleet list from destroying in-flight input
  Why: the list is replaced wholesale once a second, so text typed into a row's reply box — the headline "answer without opening the terminal" affordance — is wiped within a second, along with focus and caret.
  Evidence: `web/src/main.js:303` (`list.innerHTML = …`), driven by the 1000 ms interval at `main.js:847-850` and by every `session-updated` event at `main.js:374`.
  Touches: `web/src/main.js`
  Acceptance: rows update by mutating text nodes and attributes against a keyed diff, never by replacing the container; a row with focus or a non-empty reply field is never replaced; a test types into a reply box, drives 5 s of status updates, and asserts text and caret survive.
  Complexity: M

- [ ] R-39 · P0 — Carry PTY output as bytes end to end
  Why: each 8 KiB read is lossily decoded in isolation, so any multi-byte sequence straddling a read boundary becomes U+FFFD permanently — and both agents emit box-drawing and emoji constantly.
  Evidence: `crates/terminalai-core/src/registry.rs:1204` (`RegistryEvent::Output` carries `String`); replay at `terminalai-daemon/src/lib.rs:612,621` slices the ring at an arbitrary byte offset, so every reattach opens with mojibake. `grid.rs:524` already tests split UTF-8; the output path has none.
  Touches: `registry.rs`, `terminalai-daemon/src/lib.rs`, `crates/terminalai-app/src/main.rs`, `web/src/main.js`
  Acceptance: event and wire types carry `Vec<u8>`; the frontend receives bytes over a Tauri `Channel<InvokeResponseBody>` using `Raw`, batched on an ~8–16 ms tick (events are documented as JSON strings delivered by evaluating JavaScript, without ordering guarantees); a test feeds a 4-byte codepoint split across two chunks and asserts it arrives intact.
  Complexity: M

- [ ] R-40 · P0 — Contain and reap the whole agent process tree
  Why: agents spawn node, git, ripgrep and MCP servers; killing only the direct child orphans grandchildren holding the cwd, the reserved ports and the API session.
  Evidence: `crates/terminalai-core/src/pty.rs:188-193` (`Child::kill` = `TerminateProcess` on one PID); no `AssignProcessToJobObject` or `CREATE_NEW_PROCESS_GROUP` in the workspace; `PtySession` has no `Drop`; `environment::run_teardown` never runs on daemon exit.
  Touches: `pty.rs`, `registry.rs`, `terminalai-daemon/src/lib.rs`
  Acceptance: each child is assigned to a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; `PtySession::drop` closes it; daemon shutdown runs teardown hooks and releases port reservations; a test spawns a child that spawns a grandchild and asserts both are gone after `kill()`.
  Complexity: M

- [ ] R-41 · P0 — Prove the environment allowlist actually runs the agents
  Why: the allowlist added for R-01 is now too narrow — Node-shipped agents and anything writing under `%APPDATA%` will misbehave, and on a corporate network every request fails.
  Evidence: `crates/terminalai-core/src/environment.rs:259-275` omits `APPDATA`, `LOCALAPPDATA`, `HOMEDRIVE`/`HOMEPATH`, `TMP`, `SystemDrive`, `windir`, `NUMBER_OF_PROCESSORS`, `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`. The pty tests use `cmd.exe`, so nothing proves a real agent starts.
  Touches: `environment.rs`, `crates/terminalai-core/tests/`
  Acceptance: the allowlist covers those variables, proxies opt-in-by-presence; a test spawns the real resolved `claude.exe --version` and `codex.exe --version` through the sanitized environment and asserts zero exit; a sentinel secret in the parent is still absent from the child.
  Complexity: S

### P1

- [ ] R-42 · P1 — Take the store snapshot off the output path
  Why: the expensive part is built at output rate rather than write rate, under the global lock that also serves every IPC read.
  Evidence: `registry.rs:1208` emits `SessionUpdated` per chunk → `bridge_store` (`terminalai-daemon/src/lib.rs:300-312`) → `store_snapshot()` (`registry.rs:281-297`) clones every `Session` and every session's whole 512 KiB ring (`entry.scrollback.to_vec()`); `Vec<u8>` serializes as a JSON integer array (~3 bytes per byte), so three live sessions rewrite 4–6 MB every 200 ms.
  Touches: `registry.rs`, `terminalai-daemon/src/persistence.rs`
  Acceptance: the persisted model excludes scrollback bytes or stores them out-of-line per session; the snapshot is built inside the debounce, not per event; a test asserts no full-ring clone occurs on the output path.
  Complexity: M

- [ ] R-43 · P1 — Fix the persistence debounce and coalescer
  Why: under sustained output the debounce never fires at all, and the coalescer discards the newest snapshot rather than the oldest — so the durable file lags arbitrarily far behind reality, exactly when state matters.
  Evidence: `crates/terminalai-daemon/src/persistence.rs:87-89` (`recv_timeout` inside `while let` resets the 200 ms window on every arrival); `persistence.rs:69-72` (`try_send` on `sync_channel(1)` discards on `Full`, keeping the older queued value).
  Touches: `terminalai-daemon/src/persistence.rs`
  Acceptance: a maximum write interval is enforced regardless of event rate; the newest snapshot always wins; a test drives events faster than the debounce for 2 s and asserts at least one write occurred reflecting the final state.
  Complexity: S

- [ ] R-44 · P1 — Compute the row's last line without copying the ring
  Why: the entire 512 KiB ring is copied and lossily decoded per output chunk, per session, inside the global mutex.
  Evidence: `registry.rs:1321-1328` — `to_vec()` plus `from_utf8_lossy` plus a full split, when only the tail after the last newline is needed.
  Touches: `registry.rs`
  Acceptance: last-line extraction scans backwards from the ring tail with a bounded window; tested across a chunk boundary and with no newline present.
  Complexity: S

- [ ] R-45 · P1 — Implement scroll regions and prove the grid against real agent output
  Why: DECSTBM is a no-op, so every scroll acts on the whole screen — and both target agents pin a status/input region with margins, so background grids smear exactly where the useful state is.
  Evidence: `Handler::set_scrolling_region` is never implemented (impl ends `grid.rs:455`); `linefeed`/`scroll_up`/`scroll_down`/`reverse_index` at `grid.rs:71-107,408-414`. Also `ClearMode::Saved` (`ESC[3J`) blanks the visible screen (`grid.rs:187`); no `TerminalGrid::resize`, so `registry.rs:523` resizes the PTY only and the grid stays 40×120; `Handler::input` (`grid.rs:252`) gives every char one cell so CJK/emoji shift the line; DECAWM and custom tab stops ignored (`grid.rs:321-326`).
  Touches: `crates/terminalai-core/src/grid.rs`, `registry.rs`, `crates/terminalai-core/tests/`
  Acceptance: DECSTBM, `ESC[3J`, grid resize and wide-character width implemented and tested; an Alacritty-style ref-test harness records real Claude Code and Codex byte streams once and asserts the resulting grid on every run; a `proptest` asserts arbitrary bytes never panic and never move the cursor out of bounds.
  Complexity: L

- [ ] R-46 · P1 — Survive a poisoned registry lock
  Why: one panic under the lock permanently poisons it, after which the daemon still accepts connections and still owns live agents but fails every request, with nothing detecting it.
  Evidence: `.expect("registry poisoned")` at `registry.rs:300-306,992,1215` and ~40 further sites.
  Touches: `registry.rs`, `terminalai-daemon/src/lib.rs`
  Acceptance: lock acquisition recovers the guard or maps poisoning to a typed error surfaced to the client and to diagnostics; a test poisons the lock and asserts the daemon still answers a list request with an explicit error.
  Complexity: M

- [ ] R-47 · P1 — Bound the event queues and fix the writer-thread leak
  Why: a stalled WebView accumulates a full `Session` clone per output chunk with no drop policy, and every client disconnect leaks three threads plus the connection.
  Evidence: unbounded `mpsc::channel()` at `registry.rs:301` and `terminalai-daemon/src/lib.rs:324`; `write_messages` blocks on a full pipe (`lib.rs:432-438`); the event bridge holds a clone of `outgoing_tx` (`lib.rs:415`) so `writer.join()` (`lib.rs:426-428`) never returns after the handler drops its sender.
  Touches: `registry.rs`, `terminalai-daemon/src/lib.rs`
  Acceptance: queues are bounded with an explicit coalesce-or-drop policy for `Output` and `SessionUpdated`; drops are counted and visible in diagnostics; the writer thread terminates on disconnect and a test asserts stable thread count across 100 connect/disconnect cycles.
  Complexity: M

- [ ] R-48 · P1 — Make notification retraction actually retract
  Why: a session moving between attention states leaves a live badge forever, and removed sessions keep theirs — the failure mode the dedup work was meant to prevent.
  Evidence: `crates/terminalai-core/src/notification.rs:115-122` — `retract_session` removes one key while `dedup_key` includes the status (`notification.rs:144-148`); `registry.archive`/`remove_entry` (`registry.rs:499,997`) never retract, so `NotificationCenter.active` grows.
  Touches: `notification.rs`, `registry.rs`
  Acceptance: retraction is keyed by session id and clears every entry for that session; archive, kill and remove all retract; a test transitions NeedsApproval → AwaitingInput → Idle and asserts zero active notifications.
  Complexity: S

- [ ] R-49 · P1 — Restart scheduling must not spawn a thread per attempt
  Why: when admission is full, `restart` reschedules itself every 250 ms by spawning a new OS thread, indefinitely, without consuming the restart budget.
  Evidence: `registry.rs:1079-1087,1103-1106` — backoff is not applied on this path and `MAX_RESTARTS` is not consumed because `schedule_restart_at_from` is not called; `let _ = thread::Builder::spawn` silently drops the restart on failure, leaving the session in `Backoff` with nothing scheduled.
  Touches: `registry.rs`
  Acceptance: one timer thread or scheduled-wakeup queue services all pending restarts; admission-blocked restarts consume backoff and budget; a failed spawn transitions to `Failed`; a test fills admission and asserts thread count stays constant.
  Complexity: M

- [ ] R-50 · P1 — Move blocking setup/teardown hooks off the supervision path
  Why: a 30-second hook blocks `launch`, `revive`, `drain_queue` (on the PTY monitor thread) and `kill()` (an IPC handler), so one slow hook stalls unrelated sessions and the UI.
  Evidence: `registry.rs:895,1052`; `crates/terminalai-core/src/environment.rs:189-217` — `HOOK_TIMEOUT` is polled at 25 ms with no cancellation, and the timeout path does not kill the hook's `cmd.exe` tree.
  Touches: `environment.rs`, `registry.rs`
  Acceptance: hooks run on their own worker with the session shown in a `preparing`/`tearing-down` state; a timed-out hook has its whole process tree killed (reuse R-40's job object); no IPC handler blocks on a hook.
  Complexity: M

- [ ] R-51 · P1 — Bound and time-limit the review surface
  Why: one repo with a stuck `index.lock` hangs the entire review request permanently, and a large diff is materialized twice before truncation.
  Evidence: `crates/terminalai-core/src/review.rs:88-99` — three serial `git` subprocesses per session via `Command::output()` with no timeout; `registry.rs:325-346` loops up to 30 sessions → 90 spawns per request; truncation to 128 KiB happens after full buffering (`review.rs:92,97`); `count_conflict_markers` (`review.rs:154`) counts any added `=======` line, adding 1000 to `review_cost` per false positive.
  Touches: `review.rs`, `registry.rs`
  Acceptance: per-repo timeout with a partial result and a visible "timed out" state; diffs read incrementally and capped during read; git invocations run concurrently with a bounded pool; conflict detection requires the full `<<<<<<<`/`=======`/`>>>>>>>` triple.
  Complexity: M

- [ ] R-52 · P1 — Correct the fleet list's interactive semantics
  Why: rows are `tabindex="0"` with `role="listitem"`, which is invalid for an interactive element and leaves assistive technology without position or selection information.
  Evidence: `web/src/main.js:372` (`role="listitem" tabindex="0"`) inside `index.html:41` (`role="list"`). The Enter/Space handler landed at `main.js:313`, so activation works; the roles and set metadata did not.
  Touches: `web/src/main.js`, `web/index.html`, `web/src/styles.css`
  Acceptance: the list is `role="listbox"` with `role="option"` rows and roving tabindex (`role="grid"` is rejected — see RESEARCH.md); each row exposes `aria-posinset`/`aria-setsize` and `aria-selected`; per-row actions are real buttons; `needs-approval` and `needs-you` stop sharing the `!` glyph.
  Complexity: M

- [ ] R-53 · P1 — Announce transitions, not the summary, and stop the reorder hazard
  Why: the summary is a live region rewritten every second, so a screen reader recites it continuously while the events that matter go unannounced — and attention-sorting moves rows under the user's cursor.
  Evidence: `web/index.html:19` marks `#fleet-summary` `aria-live="polite"`; `main.js:271` rewrites it from the 1 s interval. A dedicated `#fleet-announcer` region exists (`index.html:42`) but the summary was not demoted. Reordering under focus conflicts with WCAG 2.4.3; xterm.js caps its own live region at 20 rows for the same reason.
  Touches: `web/index.html`, `web/src/main.js`
  Acceptance: `#fleet-summary` is no longer a live region; `#fleet-announcer` announces only actionable transitions coalesced over ≥2 s ("2 sessions need you: api-refactor, docs"); reordering freezes while the list has focus or hover, with pending moves surfaced as an explicit "N sessions changed priority — Apply" affordance that preserves focus on the same session id.
  Complexity: M

- [ ] R-54 · P1 — First-run preflight and a visible daemon-unreachable state
  Why: if the daemon cannot start, `setup` returns `Err` and the app exits with no console and no window — the user sees nothing at all.
  Evidence: `crates/terminalai-app/src/main.rs:342-357,420-423`; the `resolve_agent` command (`main.rs:106`) is exposed but never called from the frontend; no loading state between invoke and resolve in `renderReview`/`loadSnapshot`.
  Touches: `crates/terminalai-app/src/main.rs`, `web/src/main.js`, `web/index.html`
  Acceptance: a preflight panel in the `flutter doctor` shape — one row per dependency (claude CLI + version, codex CLI + version, hooks installed, daemon reachable, Start-Menu shortcut with `System.AppUserModelID`), each with detected value, inline Fix and Recheck; daemon failure shows this panel instead of exiting; the same component is reachable later as Diagnostics.
  Complexity: M

### P2

- [ ] R-55 · P2 — Move advisory checking to cargo-deny with target filtering
  Why: 11 of the 17 current warnings are GTK3/glib crates that never compile on Windows, so they are noise that trains the team to ignore the report.
  Evidence: reverse-dependency analysis of `Cargo.lock` — RUSTSEC-2024-0411…0420 and 0429 reach the graph only under `cfg(linux/*bsd)`; RUSTSEC-2024-0370 via `glib-macros`/`gtk3-macros`. The five `unic-*` advisories (RUSTSEC-2025-0075/0080/0081/0098/0100) DO ship, via `unic-ucd-ident` ← `urlpattern` ← `tauri-utils`. Tauri's GTK4 migration (tauri#7335, tauri#12563) is open with no date.
  Touches: `deny.toml`, release checklist
  Acceptance: `cargo deny --target x86_64-pc-windows-msvc check advisories` passes with `vulnerability = "deny"` and exactly five `[advisories.ignore]` entries, each with a written `reason` and upstream link; the release gate runs it.
  Complexity: S

- [ ] R-56 · P2 — A UI test and screenshot path that actually works
  Why: the repo cannot satisfy its own "re-capture screenshots on UI change" rule — the visual-isolation launcher places the window on a private desktop that display capture cannot reach.
  Evidence: verified 2026-08-02 — the app launched, `verify` confirmed placement at 1454×832, but `screenshot` captured only the desktop wallpaper. `@wdio/tauri-service` 1.2.0 runs an embedded WebDriver server in the app, keeps Edge WebDriver in sync on Windows and adds IPC command mocking; there is no headless WebView2.
  Touches: new test harness, release checklist
  Acceptance: `@wdio/tauri-service` drives the built app, captures the fleet list, launcher, review and diagnostics views, and asserts the empty, loading, error and daemon-unreachable states render; IPC mocking supplies a fake fleet.
  Complexity: M

- [ ] R-57 · P2 — Correct terminal sizing and the focus-switch race
  Why: the terminal is hard-coded to 120×40 regardless of pane size, and output arriving during a focus switch is written to the wrong session.
  Evidence: `web/src/main.js:721-724` instantiates and registers `FitAddon` but never calls `.fit()`, and there is no window `resize` listener. `main.js:462-471` assigns `state.focused` after the awaited `reattach`; `hydrateTerminal` (`main.js:480-492`) resets then writes across another await.
  Touches: `web/src/main.js`
  Acceptance: the terminal fits its pane, with resize debounced into an explicit PTY resize request and never fired on splitter drag (agent TUIs hard-wrap and do not reflow, so a drag-driven resize corrupts the output being parsed for status); focus switches carry a generation token so late output for the previous session is discarded; replay uses DEC 2026 synchronized output and `onWriteParsed` (both new in `@xterm/xterm` 6.0.0, already pinned) to avoid tearing.
  Complexity: M

- [ ] R-58 · P2 — Harden the IPC message boundary
  Why: either end can be OOM-ed by a peer that sends bytes without a newline, and a malformed frame tears down the connection with no reply.
  Evidence: `terminalai-daemon/src/lib.rs:336,770` grow a `String` with no cap; `Request::Write { data: String }` (`lib.rs:106`) is unbounded; malformed JSON drops the connection at `lib.rs:339-340`; a transient `thread::Builder::spawn` failure inside the accept loop terminates `serve()` entirely (`lib.rs:281`), abandoning every live session.
  Touches: `terminalai-daemon/src/lib.rs`
  Acceptance: a maximum frame size is enforced on read with a typed error response; oversized `Write` payloads are rejected, not truncated; malformed JSON returns `Response::Error` and keeps the connection; a spawn failure logs and continues accepting.
  Complexity: S

- [ ] R-59 · P2 — Daemon lifecycle: shutdown, skew diagnosis, no duplicate spawn
  Why: nothing ever stops the daemon, and on protocol skew the app spawns a second one that cannot bind, then reports a generic timeout while the old daemon keeps running with live agents and no UI.
  Evidence: `serve()` (`lib.rs:270-285`) loops until killed — no console-control handler, no idle or last-client shutdown; the client's typed `VersionMismatch` arm (`lib.rs:690`) is dead code because the daemon answers `Response::Error` (`lib.rs:367-380`); `crates/terminalai-app/src/main.rs:261-300` treats any connect failure as "no daemon running"; `PIPE_NAME` embeds `v2` (`lib.rs:38`), so a future v3 orphans the v2 daemon permanently. `interprocess` already sets `FILE_FLAG_FIRST_PIPE_INSTANCE`.
  Touches: `terminalai-daemon/src/lib.rs`, `main.rs`, `crates/terminalai-app/src/main.rs`
  Acceptance: version mismatch surfaces a distinct actionable message naming the running daemon and how to stop it, and does not trigger a spawn; the daemon exposes a graceful shutdown request and a console-control handler; a single-instance guard prevents duplicate spawns.
  Complexity: M

- [ ] R-60 · P2 — Describe the trust boundary honestly and tighten the DACL
  Why: the peer check is self-consistency rather than authorization, and the DACL broadens under elevation — while the control pipe can run arbitrary shell commands.
  Evidence: `terminalai-daemon/src/lib.rs:322,381` compares `GetNamedPipeClientProcessId` against a client-declared `client_pid`; `lib.rs:821` grants `GA` to `OW`, which resolves to `Administrators` when an elevated process's token default owner is that group; `crates/terminalai-core/src/environment.rs:220-233` runs setup/teardown via `cmd.exe /c` from any pipe client's `LaunchSpec`.
  Touches: `terminalai-daemon/src/lib.rs`, `environment.rs`, `CLAUDE.md`, `README.md`
  Acceptance: the DACL names the interactive user's SID explicitly rather than `OW`; `IpcError::PeerMismatch` and the module docs state that the DACL is the boundary and the PID is diagnostic; shell hooks are opt-in per session and documented as local code execution.
  Complexity: S

- [ ] R-61 · P2 — Close the CSP and clipboard gaps
  Why: `base-uri` and `form-action` do not inherit from `default-src`, and agent output reaching the clipboard is a threat rather than a feature.
  Evidence: `crates/terminalai-app/tauri.conf.json:15` sets only `default-src`, `connect-src`, `img-src`, `style-src 'unsafe-inline'` and `script-src`; `@xterm/xterm` 6.0.0 added OSC 52 clipboard support; `main.js:362` is the one attribute interpolation bypassing `escapeHtml`.
  Touches: `tauri.conf.json`, `web/src/main.js`
  Acceptance: CSP adds `base-uri 'none'` and `form-action 'none'`; OSC 52 writes are disabled or require explicit per-session opt-in; the unescaped interpolation goes through `escapeHtml`.
  Complexity: S

- [ ] R-63 · P2 — Structured logging with bounded retention
  Why: the diagnostics timeline explains one session, but nothing records what the daemon did across sessions, and the crash log grows without limit.
  Evidence: `crates/terminalai-daemon/src/persistence.rs:48-51` appends to `crash.log` with no rotation; there is no `tracing` subscriber anywhere in the workspace, so a status misattribution — the dominant bug class in this field — leaves no trail beyond the in-memory timeline.
  Touches: `terminalai-daemon`, `terminalai-core`, GUI log panel
  Acceptance: `tracing` with one span per session carrying `session_id`/`agent`/`cwd`; `tracing-appender` with `Daily` rotation and `max_log_files` (there is no size-based rotation — tokio-rs/tracing#1940) writing under `%LOCALAPPDATA%\TerminalAI\logs\`; the `WorkerGuard` is held in `main` so the tail including panics is not lost; an in-app panel reads from a bounded `VecDeque` and is pushed to the WebView in batches on a ≥100 ms timer, never per event; `std::panic::set_hook` is installed before any thread spawns and covers PTY reader threads.
  Complexity: M

- [ ] R-62 · P2 — Finish the contrast and forced-colors work R-27 did not cover
  Why: R-27 was closed after the keyboard and labelling changes landed, but the measured contrast failures and high-contrast support were never addressed, so the acceptance criterion "contrast meets WCAG AA" is still unmet.
  Evidence: measured 2026-08-02 on Catppuccin Mocha — `overlay1` on `base` = 4.44 (fails AA by 0.06); `overlay0` (#6c7086) on `base` ≈3.8 and is still used for 9–10px text in `.row-folder`, `.eyebrow`, `.terminal-statusbar`, `.terminal-path`, `.empty-state p`, `.diagnostics-heading p` (`web/src/styles.css:39,57,61,63,71`); `--surface2` is used as text in `.terminal-grid` and `.terminal-placeholder small` (≈2.8:1); accents on `surface1` = 3.94–4.49, so selected rows must use `surface0`. No `@media (forced-colors: active)` or `prefers-reduced-motion` block exists anywhere in the stylesheet; `-ms-high-contrast` is dead as of Edge 138 and WebView2 is Chromium, so colour-only status vanishes in High Contrast.
  Touches: `web/src/styles.css`, `web/src/main.js`
  Acceptance: no text renders below 4.5:1; `overlay0`/`overlay1` are decorative only; selected rows use `surface0`; a `forced-colors` block maps to system keywords with `forced-color-adjust: none` limited to the xterm surface; `prefers-reduced-motion` disables the pulse and glow animations; xterm `screenReaderMode` is an opt-in setting (it breaks right-click copy/paste, xterm.js#1931).
  Complexity: M

- [ ] R-28 · P2 — Distribution and update flow
  Why: unsigned Windows distribution has a known SmartScreen path and a WebView2 decision that changes installer size by ~180 MB.
  Evidence: https://v2.tauri.app/distribute/windows-installer/ — WebView2 is Evergreen-preinstalled on Win10 21H2+/Win11, so `downloadBootstrapper` is correct; `fixedRuntime` adds ~180 MB.
  Touches: `crates/terminalai-app/tauri.conf.json`, release process
  Acceptance: unsigned NSIS/MSI built locally, `downloadBootstrapper` mode, documented SmartScreen "More info → Run anyway" step, and an in-app update check that never auto-installs.
  Complexity: S
  — *2026-08-02 research: `downloadBootstrapper` is already set (`tauri.conf.json:34-37`), so only the update check remains. `tauri-plugin-updater` 2.10.1 requires a signature that "cannot be disabled" — but that key is a self-generated minisign keypair from `tauri signer generate`, not Authenticode and not a CA certificate, so it does NOT conflict with the project's no-code-signing rule. Store the private key outside the repo and document that losing it orphans installed clients.*

- [ ] R-29 · P2 — Versioned session store with a migration path
  Why: session state will outlive its schema, and a daemon that survives GUI upgrades will meet older files.
  Evidence: Zellij versions its session-info directory by release; RESEARCH.md "Architecture".
  Touches: `store.rs`, `terminalai-daemon/src/persistence.rs`, test fixtures
  Acceptance: the file carries a magic string plus an integer version (SQLite `application_id`/`user_version` shape); older versions are copied to `sessions.v<old>.bak` before migrating; unknown fields survive a round-trip via `#[serde(flatten)]`; one fixture per historical version asserts migration to the current shape.
  Complexity: S
  — *2026-08-02 research: depends on R-34 (atomic writes) and R-36 (quarantine). As written this item assumes the failure paths work; today a version mismatch is indistinguishable from corruption and both brick the daemon, so do those first. Chrome's `Last Version` is the precedent for refusing unknown-newer loudly rather than best-effort parsing.*

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
