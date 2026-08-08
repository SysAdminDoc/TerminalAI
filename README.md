# TerminalAI

[![version](https://img.shields.io/badge/version-0.23.0-blue.svg)](CHANGELOG.md)
[![license](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![platform](https://img.shields.io/badge/platform-Windows%20x86__64-lightgrey.svg)](#requirements)
[![rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)

A control surface for running many Claude Code and Codex sessions at once.

Pick the agent, model, effort, permission mode and project folder from a GUI; TerminalAI
launches the real CLI on a pseudo-console and puts it in a dense fleet list. Twenty-nine tracked
sessions fit on one screen at the default 1440x900 window because only the ones you are actually
looking at get a terminal grid — the rest are a status row.

## Why another one

Every multiplexer in this space renders **panes**. Panes do not stack: four xterm panes fill a
1440p display. TerminalAI splits the problem in two.

| | Needs | Cost |
|---|---|---|
| **Attention data** — busy? blocked on approval? which model? how long? | ~28px | a ring buffer |
| **Interaction surface** — the actual TUI | ~600px | a terminal renderer |

You only ever type into one session at a time, so only one session pays for a renderer
(plus any you pin). Everything else is a row.

The compact row is measured, not aspirational: 28px exactly, carrying the status glyph, session
name, repository, agent, tool progress, restart count, dwell time and last output line on one
line. Measured 2026-08-03 in WebView2's engine at the default 1440x900 window, that is 29 fully
visible rows. **Wide** adds a second line with model, reasoning effort, branch, ports, the
spelled-out status and reported cost, at 50px.

Thirty is a tracked-session target, not a promise to keep thirty agent processes hot. Local
measurements on 2026-08-02 showed roughly 509 MB RSS for Claude Code and 322 MB for Codex per
live process; real work should stay within the machine's memory budget, typically two or three
parallel agents. What the daemon does with an idle session is demote it rather than unload it:
every process in an unfocused, unpinned session's job drops to Windows EcoQoS and low memory
priority, and focusing or pinning restores it. A row that leads an agent team names its teammates,
because one row can be a lead plus several separate agent instances and density is only a virtue
while a row's cost stays legible. Hibernating a session and rehydrating it on demand
is not implemented — it is parked pending live evidence that `--resume` restores enough context to
make it transparent.

Status comes from agent hooks and transcript tailing — pushed, not polled. Polling a pty on a
timer, which is the common approach, both misses transitions between ticks and burns CPU per
session. Exit detection blocks on the child's own process handle for the same reason: measured on
2026-08-03 with `terminalai-probe cpu-idle --sessions 10 --seconds 60`, supervising ten idle
sessions cost 218.8 ms of CPU per minute when polling at 50 ms and less than one scheduler tick
when waiting on the handle.

## Status

**v0.23.0 — core and desktop shell built, installed, and verified end to end.**

Working today (`terminalai-probe`, headless):

- Resolves the real `claude.exe` / `codex.exe` behind the npm shims
- Builds the exact argument vector for any launcher combination, per agent
- Spawns and drives either agent on a live ConPTY
- Runs a Tauri 2.11.5/WebView2 shell with the Catppuccin fleet list, launcher, presets and one
  reused xterm renderer
- Preserves PTY and replay output as raw bytes end to end, delivering focused-session batches to
  xterm without corrupting multi-byte sequences split across reads
- Windows PTY sessions use kill-on-close job objects so stopping a session reaps its descendants;
  daemon shutdown runs active teardown hooks once
- Separates "is the process alive" from "is it still working": pty output, transcript growth and
  hook events extend a per-session progress deadline, and three consecutive misses mark a session
  unresponsive without restarting it — only a proven-dead process is restarted
- Background ConPTY sessions use Windows EcoQoS and low memory priority while neither focused nor
  pinned; focus or pinning restores normal priority. The policy is applied to every process in the
  session's job, not only the one being supervised — since agent teams a row can be a lead plus
  several separate agent instances, and demoting the lead alone would leave the rest at foreground
  priority. Memory is read the same way, summed across the job the per-session cap is enforced
  over, and the row says how many processes the figure covers. Waiting-session counts appear as a numeric
  Windows taskbar overlay.
- Native Claude and Codex version checks run through the same sanitized environment allowlist as
  live sessions, including opt-in proxy variables without inheriting parent secrets
- Lets a launch bound its own fan-out — the concurrent-subagent cap and whether agent teams are
  allowed at all — because admission governs how many sessions run, not how many agents one session
  is. Codex, which documents no equivalent, is refused rather than launched as if it had one
- Ships an installer that actually starts: the NSIS and MSI bundles carry the daemon and probe as
  sidecars, and `scripts/verify-installer.ps1` installs into a scratch prefix, launches the
  installed binary on an isolated display, asserts the window and the daemon pipe, then uninstalls
- Tails each live session's transcript for its own session id, last message and real cost, reading
  only the bytes appended since the last poll
- Lands a session's work into its repository through a serialised gate that refuses whole — naming
  the specific condition — rather than half-applying
- Reads a per-repository environment lease (`.terminalai/environment.toml`) covering untracked
  config, a docker compose project prefix and a Postgres database cloned per session
- Exposes the fleet to an MCP client over stdio, read-only unless a write token and opted-in
  sessions are supplied
- Lets one agent wait until another genuinely needs input, instead of polling and guessing.
  `await_session` is a read — no write token, and it never types into a session, wakes one or
  answers a prompt. It returns immediately when the condition is not met yet, saying how long is
  left and how soon to ask again, because the server reads stdio on one thread and a tool that
  slept would stall every other agent's read on it
- Keeps a bounded in-memory ring per session over a rotating on-disk log, so history outlives the
  ring and survives a daemon restart
- Gives a session its own Git worktree and branch on request, cleaned up with the row and never at
  the cost of unmerged commits
- Breaks fleet spend down by agent, project folder and session, counting unpriced sessions apart
  rather than as zero
- Sends one prompt to many sessions, reporting each separately and refusing those waiting on a
  permission decision
- Reads launch templates a repository declares about itself (`.terminalai/templates.toml`)
- Registers a root once and turns every Git repository under it into a launch target, with a view
  of which ones still have open roadmap items
- Queues prompts against a busy session, advancing on the reported status and pausing rather than
  answering a permission prompt blind
- Runs one stored prompt across many projects, flagging repositories with uncommitted changes
  instead of launching into them
- Lists every session waiting on a permission decision in one place, longest wait first, with the
  tool and arguments it is asking about. Nothing there approves anything on your behalf: no
  approve-all, no bypass mode, and the answer you type goes to that session's prompt as typed
- Finds a string in the focused pane, and across every session's retained scrollback on disk — the
  other rows have no renderer to search. Colour and cursor sequences are removed before matching,
  so what is searched is what was legible rather than the bytes that drew it
- Saves a named layout of many sessions and relaunches it as one action. Restoring goes through the
  ordinary launch path, so admission, the memory budget, the spend ceiling and the dirty-tree
  refusal all still apply; a session they refuse is reported, not forced
- Shows how full each session's context window is when that is measurable, and an em dash when it
  is not — the window is reported by the agent, never inferred. Compaction appears in the status
  history instead of looking like a stall
- 769 default Rust tests over agent identification and resolution against an injected filesystem,
  the flag mapping, real-pty boundary and blocking exit wait, supervision state machine, registry,
  diagnostics, review aggregation and reviewed-mark expiry, the land gate against real
  repositories, environment leases, transcript tailing, the MCP boundary, cost model and vendored
  price table, external-session discovery, daemon protocol and frame bounds, presets, launch golden
  fixtures, atomic file recovery, corrupt-store quarantine, deterministic fleet ordering, prompt
  safety and fleet-row model, per-session worktrees against real repositories, the two-tier
  scrollback store, repository-declared templates, the broadcast eligibility rule, project
  discovery and roadmap scanning against this machine's own repositories, the prompt queue's state
  machine and the work queue's dirty-tree refusal, the fleet spend ledger and its admission ceiling,
  memory-aware admission and job limits, agent authentication state, the bounded session
  archive and the leftover-checkout survey, the progress deadline that separates a silent
  session from a busy one, both MCP protocol eras, and the landing record that tells a finished
  session from an abandoned one, the admission gate and the restart policy as decisions
  over state passed in, the context-window reading that is the last request's prompt rather than
  the running total, the escape-stripping search over retained output, the wait primitive one agent
  blocks on to reach another, origin mode against the conformance corpus, the working-directory
  change that invalidates a row's folder and branch, and the DPI and restart declarations made
  against the real Windows APIs, the admission limits read from the operator's configuration, and
  the quota window attributed to the sessions that consumed it; 772 with the opt-in app-server
  transport enabled, plus 398 frontend tests (`npm --prefix web test`) and a real browser pass over
  every dialog, menu and disclosure across a populated fleet in both row densities
  (`npm --prefix web run test:chrome`)

`Roadmap_Blocked.md` records what is waiting on something external. The experimental
Codex app-server adapter is available only when the daemon is built with the explicit
`codex-app-server` feature; the default daemon continues to use hooks and the PTY path. The named-pipe
daemon keeps live sessions independent of the window; see
[ROADMAP.md](ROADMAP.md).

The daemon writes a versioned, human-readable session store to
`%LOCALAPPDATA%\TerminalAI\sessions.json` and raw replay tails to its adjacent
`sessions.json.scrollback` directory. If the file is unreadable or has an unsupported schema,
the daemon moves it to a timestamped `sessions.corrupt-...json` quarantine and starts with an empty
fleet; the window shows the quarantine path in a dismissible banner. Reconnecting the window reattaches to live rows and
replays bounded scrollback; rows recovered after a daemon restart remain stopped until the operator
chooses native resume or archive.

Session metadata persists after a 200 ms quiet period and at least once per second during sustained
output, so a busy fleet cannot indefinitely delay its newest durable state.

Each session retains the latest 64 status transitions with their timestamp and source, and the
terminal panel's diagnostics view explains why the focused row is in its current state. Daemon and
desktop-shell panics enter the structured rolling log under
`%LOCALAPPDATA%\TerminalAI\logs\`; fourteen daily files are retained, and the
focused terminal's log panel keeps the latest 256 records in memory.

Fleet rows expose every status with a distinct glyph and visible text label, remain keyboard
navigable, announce live status changes to screen readers, and use high-contrast tokens in both
the dark and OS light palettes. Reduced-motion and forced-colors preferences are respected. The
focused terminal toolbar includes an explicit opt-in screen-reader mode; xterm's mode changes its
right-click copy/paste behavior, so it is never enabled implicitly.

The top-bar **Check updates** action reads the latest GitHub release metadata only. It reports a
new version when one exists but never downloads, launches, or installs an update automatically.

Attention notifications are deduplicated per session and status, grouped by repository, retracted
when the agent proceeds, and quiet during startup or the first seconds of a tool call.

The daemon admits three live processes by default. Set `TERMINALAI_MAX_LIVE_SESSIONS` to change the
cap and `TERMINALAI_DEFAULT_BUDGET_USD` to change the per-session spend cap applied to launches that
did not name one (or `none` to disable it).

That cap is held by TerminalAI, not by the agent. Claude Code's `--max-budget-usd` documents itself
as working under `--print` only, and every session supervised here is interactive, so the flag is
never emitted — it would be accepted and ignored, which is the one failure mode a spend control must
not have. Instead the cap is read against the transcript-derived ledger, which covers both agents: a
session that reaches it keeps running and keeps its scrollback, and stops being given queued or
broadcast work until the operator resumes it deliberately. The row's cost turns red and says what it
was measured against.

A per-session budget bounds one agent; nothing bounded the fleet, so twenty sessions each obeying a
$5 cap could spend $100 with every individual limit reporting itself satisfied.
`TERMINALAI_SPEND_CEILING_USD` sets a fleet-wide ceiling over a rolling window
(`TERMINALAI_SPEND_WINDOW_HOURS`, 24 by default); reaching it stops anything new from starting and
never touches a session that is already running. The ledger is persisted with the session store, so
restarting the daemon does not clear the window.

**Limits** in the toolbar edits all of this against the running daemon — live sessions, the default
session budget, the spend ceiling and window, the fleet memory budget, the per-session memory cap
and process count — and applies it without a restart. Environment variables remain the boot default,
and the dialog names the ones it started from. `terminalai-probe limits [--max-live N]` drives the
same two requests headlessly. The header names which agents a per-session budget binds and says who
enforces it, so the control never implies a hard stop nobody makes.

The fleet header shows live/queued counts, how many sessions a provider is currently
rate limiting, and when the soonest quota window reopens. Spend is derived from each session's
transcript and renders an em dash until at least one session reports one — a computed-looking
`$0.00` would be worse than saying so — with a tooltip naming the price table it was computed
against.

Prices come from a commit-pinned snapshot of LiteLLM's `model_prices_and_context_window.json`
(MIT), vendored at `crates/terminalai-core/pricing/model-prices.json` and embedded in the binary.
Nothing is fetched at runtime, so an offline machine prices identically to a connected one. The
model expresses both cache-write tiers (5-minute and 1-hour, billed at 1.25x and 2x base input),
the regional-inference premium and the priority-speed premium, because real transcript records on
this machine carry all of them.

The fleet header also shows counts for every session state. Compact rows keep the status glyph,
session name, repository, agent, tool progress, restart count, dwell time and last output line on
one 28px line; use **Wide** to reveal model, reasoning effort, branch, ports, the spelled-out
status and reported cost on a second line. Branch is read from the session directory's Git HEAD
at launch and refreshed from hook events; it is an em dash outside a repository or on a detached
HEAD rather than a guess. Tool progress is populated from the agent's own plan — Claude Code's
`TodoWrite`, Codex's `update_plan` — and is an em dash when the agent exposes no countable plan. Press `/` anywhere outside a text field to focus the fleet
filter.

Each launch can reserve a deterministic block of service ports and, only when the operator supplies
them, run setup and teardown shell commands from the project directory. These hooks are local code
execution with the same trust as the operator's account; they are not agent-sandboxing or a remote
service boundary. Hooks and the agent receive `TERMINALAI_SESSION_ID`,
`TERMINALAI_PORTS`, `TERMINALAI_PORT_BASE`, and the first port as `PORT`.

Sessions started outside TerminalAI — from a terminal, an IDE, or another tool — appear in a
separate **Elsewhere on this machine** panel, read from Claude Code's own per-PID session registry
with `claude agents --json` as the reconciliation fallback. Those rows are deliberately actionless:
TerminalAI owns none of their processes, so it offers nothing it cannot perform. Identity is
`(pid, procStart)`, which survives PID reuse. A registry that cannot be read reports "unknown",
never "idle" — reporting idle from the absence of a signal is the failure mode this whole surface
exists to avoid.

The Review view is a daemon-owned, read-only Git diff surface across all sessions with changes.
It ranks entries by file and line review cost, shows additions/deletions and conflict markers, caps
diff payloads at 128 KiB per session, and lets the operator mark a session reviewed. It never stages,
resolves, or commits changes.


## Installing an unsigned build

Every release here is **unsigned**, deliberately and permanently. That is a real thing you will
meet, so here is what it looks like rather than a surprise at the point of install.

**SmartScreen.** Running a downloaded installer raises "Windows protected your PC". The way through
is *More info* → *Run anyway*. There is no version of this that eventually stops happening:
Microsoft builds SmartScreen reputation **per file hash**, and states that reputation "cannot
transfer from previous versions unless both were signed using the same publisher identity". Every
release is a new hash with no history, so every release starts from zero. Nobody should tell you it
will settle down after a few downloads — for an unsigned build it will not.

**A self-signed certificate would change nothing.** Microsoft rates self-signed executables the same
as unsigned ones. Signing with a certificate no one trusts buys a longer install story and no
additional trust.

**Smart App Control.** If it is on — it is only ever on for clean installs of Windows 11, and it
cannot be turned back on once off — it blocks unsigned executables outright, and it applies to
everything that runs, not just to things you downloaded. So it can block `terminalai-daemon.exe`
*after* a successful install, which presents as the app opening and the fleet never appearing. Check
under Windows Security → App & browser control → Smart App Control. There is no workaround short of
turning it off, which is a decision about your whole machine and not one this README will push you
toward.

**Verifying what you downloaded instead.** Since there is no signature to check, check the bytes:

```powershell
Get-FileHash .\TerminalAI_x64-setup.exe -Algorithm SHA256
```

and compare against the hash published with the release. That answers "did I get the file the
release published", which is the question a signature would have answered.

## What is in the binary

An unsigned download raises a question a signature would not have answered anyway: *which crate
versions are actually in this executable?* A lockfile in this repository answers a different
question — what the source tree said at some point — and believing it requires trusting that the
artifact was built from it.

So `terminalai-daemon.exe` and `terminalai-probe.exe` are built with
[`cargo auditable`](https://github.com/rust-secure-code/cargo-auditable), which embeds a compressed
list of crate names and versions in a `.dep-v0` linker section. It travels with the file, and any
copy can be interrogated directly:

```powershell
cargo install cargo-audit --locked
cargo audit bin .\terminalai-daemon.exe
```

That reports the full dependency tree read out of the artifact, and checks it against the RustSec
advisory database. The section carries names and versions only — no timestamps, no absolute paths.

A CycloneDX SBOM ships as a release asset, one per shipped executable. Both are produced and
verified by one script:

```powershell
pwsh -NoProfile -File scripts/supply-chain.ps1
```

It refuses to write an SBOM unless every shipped binary carries its embedded manifest, because an
SBOM generated beside a binary describes the *source tree* rather than the binary — publishing one
on its own would prove nothing about what shipped.

`terminalai.exe` is built by the Tauri CLI, which does not route through `cargo auditable`, so it
carries no embedded section; the script says so rather than passing over it, and the SBOM still
covers its dependencies.

**Provenance:** these are locally built artifacts, which is
[SLSA](https://slsa.dev/spec/v1.2/) Build **L1**. L2 requires a hosted build platform, and no
claim beyond L1 is made.

### Reproducible executables

`terminalai-daemon.exe` and `terminalai-probe.exe` build byte-for-byte identically from the same
commit — which is what lets you confirm a released unsigned binary came from this source without a
signature to check. Build it yourself and compare the hash:

```powershell
pwsh -NoProfile -File scripts/verify-reproducible.ps1
```

That builds twice, cleaning the whole target directory in between so the second build cannot reuse
the first's artifacts, and compares SHA-256. It keeps both copies outside `target/` on failure, so a
difference can be diagnosed rather than merely reported.

Determinism needed one flag. The MSVC linker stamps the PE header with the clock and gives the debug
directory a fresh GUID each link, which is 20 differing bytes in a 3.6 MB executable and nothing at
all in the compiled code; `.cargo/config.toml` passes `/Brepro`, which derives both from the
content instead.

**The installers are not reproducible, and cannot be made so as templated.** Tauri's WiX template
generates a fresh `ProductCode` per build, and the NSIS output is LZMA solid-compressed. Verify the
executables, not the setup file.

## Reporting a security problem

Please report vulnerabilities privately, through
[GitHub Security Advisories](https://github.com/SysAdminDoc/TerminalAI/security/advisories/new),
rather than as a public issue. This tool supervises agent processes and holds a named pipe that
grants control of them, so a defect in that boundary should not be public before there is a build
that fixes it.

## Language

TerminalAI ships **English only**, and that is a decision rather than a gap — recorded 2026-08-07.

The Fluent catalog stays anyway, because what it buys is not translation. It is one source of truth
for two runtimes: the daemon formats the same message identifiers the renderer does, and without a
shared file every status label would exist twice and drift. It is also the only side that checks —
`fluent-bundle` treats a duplicate message identifier as an error, while the JS loader silently
takes the last definition, and a duplicate key has shipped once and was caught by the Rust side and
nothing else.

Adding a locale needs OS-preference negotiation and a documented fallback chain, neither of which
exists. A test fails if a second catalog appears, so adding one is a deliberate change to this
decision rather than a half-built mechanism nothing selects between.

## Requirements

- Rust 1.88+ (the true floor; see Build for how to re-derive it)
- Claude Code and/or Codex CLI installed
- Windows 10 1809+ for ConPTY

**Platform.** The shipped application is Windows-native: `tauri.conf.json` bundles NSIS and MSI
only, `cargo deny` graphs `x86_64-pc-windows-msvc`, and the app crate takes `windows-sys`
unconditionally. `terminalai-core`, `terminalai-daemon` and `terminalai-probe` type-check for the
`cfg(unix)` paths — `scripts/check-cross-targets.ps1` proves it on every run — so the non-Windows
story is "the core, daemon and probe compile", not "it runs there". The badge says Windows because
that is what a release produces.

Verified against Claude Code 2.1.170 and codex-cli 0.146.0 on Windows 11 26100.

**Architectures.** Releases ship **x86_64 only**. `aarch64-pc-windows-msvc` is type-checked by
`scripts/check-cross-targets.ps1` on every run and compiles clean — the Windows-specific code is
`windows-sys` calls rather than intrinsics — but no ARM64 bundle has been built or run on real
hardware, so none is published. On a Snapdragon-class Windows machine the whole fleet therefore runs
under emulation. Type-checking is not support, and an untested second architecture would be worse
than an honest single one.

## Build

```powershell
cargo build --release
cargo test
cargo test --workspace --all-features  # includes the experimental app-server adapter
npm --prefix web test
cargo deny --target x86_64-pc-windows-msvc check advisories bans licenses sources

# Build unsigned Windows NSIS and MSI installers (from the repository root).
cargo tauri build --ci --no-sign --bundles nsis,msi -- --manifest-path Cargo.toml

# Release gate: install into a scratch prefix, launch, assert the window and the
# daemon, then uninstall. Non-zero exit means do not publish.
pwsh -NoProfile -File scripts/check-cross-targets.ps1
pwsh -NoProfile -File scripts/verify-installer.ps1

# Open every dialog, both overflow menus and the launcher disclosure in a real
# browser, at 1440px and 1100px in both colour schemes, and fail if anything
# overflows its container or any composited contrast falls below WCAG AA. This
# is the half jsdom cannot check: it has no layout engine, so it cannot know how
# wide a panel is or what colour a rule finally painted. Serves the frontend
# with Vite and drives headless Chromium — no packaged app, no daemon, no
# virtual display, and separate from the blocked WebDriver suite.
npm --prefix web run test:chrome

# Does this release describe itself correctly? Checks that every declared
# version string agrees, that CHANGELOG.md has a section for it, that no version
# section repeats a subsection, and that the test counts stated above are the
# counts the suites report. verify-installer.ps1 runs this too, minus the
# suites. -SkipTests makes it a fast metadata-only check.
pwsh -NoProfile -File scripts/verify-release-metadata.ps1

# Re-derive the MSRV floor after any dependency bump. The workspace rust-version
# must be at least what this prints, or the badge promises a toolchain that cannot
# build the tree.
cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc |
  ConvertFrom-Json |
  Select-Object -ExpandProperty packages |
  Where-Object rust_version |
  Sort-Object { [version]$_.rust_version } -Descending |
  Select-Object -First 5 name, version, rust_version
```

Note that plain `cargo build --release` is only for the probe, daemon and tests: the
`terminalai.exe` it produces is a development shell that navigates to the Vite dev server
(`http://127.0.0.1:5173`) and shows `ERR_CONNECTION_REFUSED` when no dev server is running. The
shippable app binary comes from `cargo tauri build`, which enables Tauri's `custom-protocol`
feature and embeds the built frontend.

The Tauri build stages `terminalai-daemon.exe` and `terminalai-probe.exe` as sidecars and runs
the Vite frontend build automatically (`scripts/prebuild.ps1`). The app spawns the daemon from
its own directory, so a bundle without those sidecars installs cleanly and then exits before
drawing a window — `scripts/verify-installer.ps1` exists to catch exactly that, because a build
tree always has the sibling executables and never reproduces the failure.

The installer is written to
`target/release/bundle/nsis/` and `target/release/bundle/msi/`; both artifacts are intentionally
unsigned. Windows SmartScreen may warn on first launch: choose **More info**, then **Run anyway**
only if the installer came from your verified build or release source. The WebView2 dependency uses
the Evergreen `downloadBootstrapper` mode instead of bundling a fixed runtime.

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

# Drive the daemon headlessly; each --json command emits one stable JSON object.
terminalai-probe list --json
terminalai-probe start claude --cwd . --prompt "run the tests" --json
terminalai-probe status s0001 --json
terminalai-probe send s0001 "focus on failing tests first" --json
terminalai-probe stop s0001 --json
terminalai-probe shutdown

# What would this launcher choice actually run?
terminalai-probe preview claude --model opus --effort xhigh --permission plan
terminalai-probe preview codex --model gpt-5.1-codex --effort high --sandbox workspace-write

# Run it for real on a pseudo-console.
terminalai-probe spawn claude --raw --version

# Print the installed runtime's model/effort catalog and feature names.
terminalai-probe capabilities codex --json

# Run anything on a pseudo-console — isolates "the pty is broken" from
# "this agent is behaving oddly under a pty".
terminalai-probe exec cmd.exe /c "echo hello"

# Measure console churn and marker input latency on Windows (the default is 8 sessions).
terminalai-probe hygiene --sessions 8 --json --output .\hygiene.json

# Deliver one Claude/Codex hook payload without a browser-reachable listener.
echo '{"session_id":"...","hook_event_name":"Notification","notification_type":"permission_prompt"}' |
  terminalai-probe hook claude

# Preview, inspect, install, or remove the explicitly managed agent hooks.
terminalai-probe hooks preview claude --executable .\target\release\terminalai.exe
terminalai-probe hooks status claude --executable .\target\release\terminalai.exe
terminalai-probe hooks install claude --executable .\target\release\terminalai.exe
terminalai-probe hooks remove claude --executable .\target\release\terminalai.exe

# Ask the installed CLIs whether they accept — and act on — the argv the launch
# goldens pin. Also runs as the fifth claim of the release gate. A flag whose own
# help restricts it to a mode this tool does not use is reported as
# accepted-but-ignored rather than passing because it exists.
terminalai-probe verify-goldens

# The rest of the harness, in one place so nothing is reachable and unmentioned.
# `--help` is generated from the dispatch table, so it can never advertise fewer
# commands than the binary answers to.
terminalai-probe auth                      # are both agents signed in?
terminalai-probe broadcast s0001 s0002 -- "status?"   # one prompt, many sessions
terminalai-probe queue s0001 add "next task"          # queue behind a busy session
terminalai-probe pin s0001 --json          # toggle a pinned live grid
terminalai-probe grid s0001 --json         # the parsed grid for a pinned pane
terminalai-probe history s0001 --json      # output the memory ring has dropped
terminalai-probe search "TODO" --json      # find a string across every session
terminalai-probe archives --json           # sessions this supervisor finished
terminalai-probe archive s0001 --json      # retire a stopped row into the history
terminalai-probe worktrees --json          # checkouts no live session owns
terminalai-probe land --source ./worktree --target ./repo   # land work back

# Expose the fleet to an MCP client over stdio. Read-only unless both halves
# of the write gate are given.
terminalai-probe mcp
terminalai-probe mcp --write-token <token> --write-session s0001
```

The Windows hygiene probe was run on 2026-08-03 with eight sessions inside the required isolated
non-input desktop. It samples `ConsoleWindowClass` handles on that desktop; when Windows keeps a
console host non-enumerable there, it falls back to the delta of `conhost.exe` process snapshots.
Input latency is a marker round trip over redirected stdin/stdout, so it does not inject keyboard
input into any desktop. Each row contains eight latency samples; the console count is a creation
churn count, so host cleanup races can make it exceed the requested session count.

| launch mode | console-window/host creations | min input (ms) | median (ms) | max (ms) |
|---|---:|---:|---:|---:|
| TerminalAI supervised ConPTY | 0 enumerable windows | 0.1565 | 0.1694 | 0.1810 |
| Normal `CREATE_NEW_CONSOLE` launch | 10 `conhost.exe` hosts | 0.2281 | 0.2632 | 56.3870 |

`shutdown` asks the daemon to tear down its owned sessions and exit cleanly. The daemon also
handles Windows console-close and shutdown signals. Protocol compatibility is negotiated on the
stable local endpoint, so an older running daemon is reported with its PID instead of causing the
desktop shell to start a second owner of the fleet.

Hook installation is opt-in and only owns entries carrying `--terminalai-managed`; unrelated
Claude handlers and Codex `notify` commands are preserved. Use `--config <path>` to inspect or
modify a disposable settings file before touching the user-level config.

### Which MCP revisions the server speaks

Both eras, from one process — `2026-07-28` and `2025-06-18`.

The current revision deleted the `initialize` handshake. Version, identity and capabilities now
ride in each request's `_meta`, a mandatory `server/discover` reports what a server speaks, and
every result carries a `resultType`. Supporting only that would have been the tidier code and the
wrong call: the clients this server exists for are Claude Code and Codex, and they still open with
`initialize`. The specification sanctions serving both eras concurrently, so it does.

Which era answers a request is decided by the request, never guessed:

| The client sends | It is answered in |
|---|---|
| `_meta` declaring `2026-07-28` | the current revision — `resultType`, `_meta.serverInfo`, cache hints |
| `_meta` declaring `2025-06-18` | the handshake revision, because that client has no rule for `resultType` |
| `_meta` declaring anything else | `UnsupportedProtocolVersionError` (`-32022`) listing what we speak |
| `server/discover` | the current revision — no handshake revision defines that method |
| `initialize` | the handshake revision, for the life of the process |
| anything else | the handshake revision |

The last row is the compatibility guarantee: a client that says nothing sees the bytes it saw
before this server learned a second revision, and a test asserts that the handshake-era tool
listing carries no `resultType`, no `_meta` and no cache fields. `ping` is the one method that
answers in one era and not the other — `2026-07-28` removed it, so a modern client is told it is
gone rather than being served a method its own revision deleted.

`server/discover` returns the supported-version list verbatim from the same constant the
negotiation check reads, so the wire cannot drift from the code; a test pins that constant to the
revision this server was written against. Cache hints are `private`, never `public` — the tool
listing varies with which sessions the operator opted in, so a shared intermediary handing one
operator's answer to another would disclose that.

## What the launcher maps to

Verified flags, not guesses.

| Control | Claude Code | Codex |
|---|---|---|
| Model | `--model` | `--model` |
| Effort | `--effort <runtime value>` | `--config model_reasoning_effort="…"` |
| Project folder | process cwd | `--cd` |
| Extra writable dirs | `--add-dir` | `--add-dir` |
| Permission | `--permission-mode default\|plan\|acceptEdits\|bypassPermissions` | Plan: `--config collaboration_mode.mode="Plan"`; Accept edits: `--ask-for-approval on-request` with `--sandbox workspace-write`; otherwise `--ask-for-approval on-request\|never` |
| Sandbox | — | `--sandbox read-only\|workspace-write\|danger-full-access` |
| Session label | `--name` | TerminalAI-side |
| Config profile | — | `--profile` |
| Resume last | `--continue` | `resume --last` |
| Resume by id | `--resume <id>` | `resume <id>` |
| Fork | `--resume <id> --fork-session` | `fork <id>` |
| Spend cap | TerminalAI-side (ledger) | TerminalAI-side (ledger) |
| Web search | — | `--search` |
| Tools allowed / denied | `--allowed-tools` / `--disallowed-tools`, one occurrence per entry | — |
| Extra settings | `--settings`, `--setting-sources` | — |
| MCP servers | `--mcp-config` per entry, `--strict-mcp-config` | — |
| Session plugins | `--plugin-dir` / `--plugin-url` per entry | — |
| Fallback model | `--fallback-model` | — |

The Codex column of the last five rows is empty because `codex --help` 0.146.0 expresses none of
them: MCP servers are managed by the `codex mcp` subcommand and `config.toml`, plugins by `codex
plugin`, and there is no allow/deny tool list or fallback model at all. `--strict-config` is
deliberately not mapped to the strict-MCP row — it refuses unrecognised `config.toml` fields, which
is a different thing. Choosing one of these with Codex selected refuses the launch; it is never
dropped, because an allowlist that quietly disappears is a session with *more* rope than was asked
for.

The list-valued options are emitted as one flag occurrence per entry rather than as a joined
string. `--allowed-tools` accepts a comma- or space-separated list, and a tool pattern such as
`Bash(git log:*)` contains a space, so joining would re-split a value on a separator inside it.
Values are refused before argv is built if they begin with `-` — they sit beside the flags that
decide what the agent may do, and a dash-leading value there is a second option nobody chose. A
plugin URL must be `http(s)`: it fetches and runs remote code, so `file:` or `data:` there would be
a different mechanism wearing the same field's name.

Two flags are deliberately **not** mapped. `--max-turns` does not exist in Claude Code 2.1.170, so
there is nothing to map yet. Claude Code's own `-w, --worktree` overlaps this project's worktree
feature and loses to it: TerminalAI places the checkout, names the branch, refuses to adopt an
existing one, gates landing on it and surveys the ones left behind. A worktree the agent created for
itself is one the supervisor did not place and cannot reason about, so the launcher owns that
decision.

The mapping itself is data, not code: `crates/terminalai-core/agents/builtin.toml` describes each
family's identity, npm layout and every flag spelling above, and one builder emits the argument
vector in the order that file lists. An operator can override it with `agents.toml` in the
TerminalAI data directory; a repository cannot supply one, because a repo-declared agent definition
would be arbitrary argv arriving with a clone.

### What a sandbox does and does not mean here

The em dash in the Sandbox row is not "TerminalAI does not map this flag". **On native Windows
neither agent has a first-party filesystem sandbox available at all.** Claude Code's Bash sandbox
"runs on macOS, Linux, and WSL2. Native Windows is not supported"
([sandboxing](https://code.claude.com/docs/en/sandboxing)) — so there is no Claude flag to map,
and a Claude session on this machine is confined by nothing but the permission mode it was started
in. Codex's `--sandbox` is the only sandbox flag in the table, and what it constrains is Codex's own
tool calls: `read-only` refuses writes, `workspace-write` permits them under the workspace roots,
`danger-full-access` removes the check. None of the three is an OS boundary — a shell command the
agent runs still executes as the operator, with the operator's token.

What actually isolates a session here is the pair this app ships together:

- **the worktree** — a session in its own checkout and branch cannot touch another session's files,
  which is why the built-in bypass preset turns it on and why it never adopts an existing branch;
- **the environment lease** — the child gets a sanitized allowlist carrying no credential of any
  kind, plus its own port block, so a session cannot reach a token merely because the parent shell
  had one exported.

That pair is the mitigation. If a bypass session needs a real OS boundary, run TerminalAI's agent
inside a WSL2 distribution or a VM; nothing in this launcher can supply one on native Windows.

### Which authentication a session runs as

The agent inherits a sanitized environment allowlist that carries no credential of any kind, so by
default a session authenticates exactly as the agent already does on this machine — the signed-in
account in the agent's own config directory. Nothing is inherited merely by being set in the parent
process.

Two launcher fields change that, both opt-in and both per session:

- **Agent config directory** sets `CLAUDE_CONFIG_DIR` (Claude Code) or `CODEX_HOME` (Codex). Two
  sessions pointed at two directories are two accounts.
- **Inherit these variables** names parent variables one at a time — `ANTHROPIC_API_KEY`,
  `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `NODE_EXTRA_CA_CERTS`
  for a corporate TLS root, and so on. A name that is malformed, reserved to the supervisor
  (`TERMINALAI_*`, which carries the per-session hook secret), or simply unset in this process
  refuses the launch rather than starting a session quietly missing its credential.

`terminalai-probe start` takes the same two as `--agent-home <dir>` and a repeatable
`--env-passthrough <NAME>`.

A permission mode this build does not model is passed through to the chosen agent verbatim, with a
warning, exactly as an unrecognised reasoning effort already is — Claude Code has grown `auto`,
`dontAsk` and `manual` since the four offered here were written, and a closed list would silently
rewrite a saved preset that names one rather than merely failing to offer it. Repository-declared
templates keep the closed vocabulary: an operator choosing an unmodelled mode is informed consent, a
cloned `.terminalai/templates.toml` choosing one is not.

Codex has no single flag that means "accept edits". Its documented equivalent is the auto preset —
`on-request` approvals inside the workspace-write sandbox — so that is what the control maps to, and
asking for accept-edits together with the read-only sandbox is refused rather than resolved
silently: an agent told to accept edits it cannot make is a session that fails on its first write.
Codex's `untrusted` policy is deliberately not used here; it runs only known-safe reads without
asking, so it interrupts *more* than the default, not less.

Options an agent cannot express are **refused, not dropped**. Asking for a read-only sandbox and
silently getting an unsandboxed Claude session is the kind of thing that costs you a repo. Prompts
are placed after an explicit `--` option terminator, so pasted text beginning with `-` remains text.
The `extra_args` escape hatch is trusted-input-only and is never populated from prompt text.

## Where the reasoning lives

This README says what the tool does. The arguments behind the non-obvious decisions — the 28px row,
push-not-poll, unwind-not-abort, refuse-do-not-drop, the 34.8 µs containment window, bytes-not-lines
scrollback — are in the module documentation of the code that implements each one, not in a separate
design document:

```powershell
cargo doc --no-deps --open -p terminalai-core -p terminalai-daemon
```

That is deliberate. A design document is a second place to be wrong: it goes stale silently, because
nothing fails when the code stops matching it. A module's own docs sit next to the thing they
describe and are read by whoever is about to change it. Where a decision has a number behind it, the
docs give the measurement and the date it was taken.

Some starting points: `process_tree` for why a process is contained one syscall after it exists and
what escapes in the gap; `agent` for why neither CLI is spawned from `PATH`; `scrollback` for why
the limit is bytes and never lines; `admission` and `restart` for the two policies deliberately kept
free of locks, threads and clocks; `hook_config` for why this tool writes into the operator's global
agent settings and what was measured before accepting that; `logging` for why nothing the daemon
records is unbounded.

## Layout

```
crates/
  terminalai-core/    agent resolution, flag mapping, ConPTY supervision, fleet model
  terminalai-daemon/  named-pipe control plane and the only process that owns live sessions
  terminalai-app/     Tauri shell, launcher commands, preset store and app icon
  terminalai-probe/   headless harness for everything that touches the machine
web/                   Vite frontend, Catppuccin fleet surface and xterm renderer
```

The core registry supervises an injected `AgentDomain` through an object-safe `AgentSession`
contract. `LocalPtyDomain` is the default ConPTY implementation; a remote transport can provide
the same output, input, resize and lifecycle seam without exposing a local process handle to the
registry.

The web renderer and daemon share the Fluent catalog at `web/src/i18n/terminalai.ftl`. Rust
validates and formats it through `terminalai_core::Catalog`; the renderer applies the same
message IDs and uses the browser's `Intl` plural and relative-time formatters for counts and
dwell labels. Status diagnostics cross the daemon boundary as reason kinds plus arguments, so
translated prose never becomes a wire-format dependency.

Hook installation covers the lifecycle events that change a fleet row, including prompt submission,
tool failures, permission decisions, subagents, compaction and session end. Managed Claude and
Codex hooks use the command adapter, and each supervised session carries a random hook secret only
in its own agent environment; the daemon refuses cwd-based identity and resume-id rebinding.
The daemon also exposes an ephemeral loopback HTTP endpoint for explicit callers: its bearer token,
exact Host allowlist and Origin rejection are only the transport gate, and a caller must provide
the matching per-session header before a row can change. Unknown hook event names are retained in
the diagnostics stream.

The launcher discovers model and reasoning-effort options from the resolved binaries instead of
shipping a stale allowlist. Codex is queried through `model/list` and `codex features list`; Claude
reports its active model and protocol capabilities through `system/init`. Results are cached for the
resolved executable and its version banner, invalidated when that version changes, and surfaced as
runtime datalists. A user-entered model or effort that is not advertised remains free text and is
passed through with a warning.

## Licence

MIT — see [LICENSE](LICENSE).
