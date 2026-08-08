# Blocked roadmap items

## Upgrade Claude Code and land the two version-gated items

Blocked 2026-08-08 on an operator decision, not on an absence. Both version-gated items below were
blocked because their flags postdated the installed CLI, and the versions that carry them are now
published: `https://registry.npmjs.org/@anthropic-ai/claude-code/latest` reported **2.1.226** on
2026-08-08, while this machine runs 2.1.170. `--ax-screen-reader` is documented from 2.1.181 and
`--autocompact <auto|tokens>` arrived in 2.1.221.

What cannot be done here is the upgrade itself. Claude Code is the tool the operator is running,
and replacing it underneath a live session is not a drain's call to make — the item said so when it
was filed and it is still true.

What closes it: the operator upgrades, then `terminalai-probe verify-goldens` is run against the new
version (it is claim 5 of `scripts/verify-release-metadata.ps1` as of 2026-08-08, so a release run
does it), a golden fixture for the new version is added beside `claude-code-2.1.170.json`, and the
two items below move back into `ROADMAP.md` unchanged apart from their blocker notes. Read the
compaction item's corrected evidence line first: the shape it told itself to mirror was removed, and
`--autocompact` needs its own mode check before it is mapped — the same check that caught
`--fallback-model`.

Original item:

- [ ] P1 — Re-verify against Claude Code 2.1.226 and land the two version-gated items
  Why: both items in `Roadmap_Blocked.md` were blocked on flags that postdated the installed CLI, and the versions that carry them are now published — so the blocker is an upgrade decision rather than an absence.
  Evidence: https://registry.npmjs.org/@anthropic-ai/claude-code/latest reports **2.1.226** (checked 2026-08-08); this machine runs 2.1.170. `--ax-screen-reader` appears in the changelog at 2.1.208 and is documented from 2.1.181; `--autocompact <auto|tokens>` at 2.1.221. Cross-reference rather than duplicate: "Tell the agent when the operator is using a screen reader" and "Let the operator set the compaction threshold the row now displays" already exist in `Roadmap_Blocked.md` — move them back, do not re-file them.
  Touches: `Roadmap_Blocked.md`, `crates/terminalai-core/tests/fixtures/launch/` (a fixture for the new version). The two moved items carry their own Touches; do not restate them here.
  Acceptance: the operator has upgraded (their call — do not upgrade the CLI they are running mid-session), `terminalai-probe verify-goldens` passes against the new version, a golden fixture for it exists beside the 2.1.170 one, and the two blocked entries are moved into this file unchanged apart from their blocker note. The compaction item is re-read first: it models itself on `max_budget_usd`, which the P0 above removes or relabels, and `--autocompact` needs its own mode check before it is mapped.
  Complexity: M

## Let the operator set the compaction threshold

Blocked 2026-08-07: same cause as the screen-reader item — the flag postdates the Claude Code
installed here.

`--autocompact <auto|tokens>` arrived in **v2.1.221**; this machine has **2.1.170**, and
`claude --help` does not mention it. Claude Code rejects unknown flags, so mapping it would produce
an argv this machine's agent refuses, and the argv goldens cannot catch that: they assert what this
tool emits, not what the agent accepts.

The display half already shipped in v0.17.0 — the row reports context occupancy against the model's
window when the agent states one. Only the half that *sets* the threshold is blocked.

Codex's `model_auto_compact_token_limit` is a config key rather than a flag and is not
version-gated, so that half could land alone; it is held back deliberately so the feature does not
ship as "works for one agent, silently absent for the other", which is the failure mode the
`LaunchError::Unsupported` rule exists to prevent.

Unblocks by upgrading Claude Code to 2.1.221 or later — not done during an autonomous drain,
because Claude Code is the tool the operator is actively running.

Original item:

- [ ] P3 — Let the operator set the compaction threshold the row now displays
  Why: the context reading landed 2026-08-07, so the fleet reports how full a window is but cannot influence when the agent acts on it — and both agents take the threshold as a launch-time input this launcher does not map.
  Evidence: Claude Code added `--autocompact <auto|tokens>` in v2.1.221 and the `CLAUDE_CODE_AUTO_COMPACT_WINDOW` variable (100000–1000000 tokens); Codex exposes `model_auto_compact_token_limit`. `LaunchSpec` (`crates/terminalai-core/src/launch.rs:203`) maps neither; ~~`max_budget_usd` is the shape to mirror — an existing optional numeric that is Claude-only in the same way.~~ Corrected 2026-08-08: `max_budget_usd` is no longer an argv slot at all. `claude --help` restricts `--max-budget-usd` to `--print`, so the flag was removed from the manifest and the cap is enforced by this tool's ledger instead — do not copy that shape without first checking what `--autocompact`'s own help says about its mode, because the two flags may be restricted the same way. `web/src/contextPressure.js` already has the cell that would show the threshold beside the usage.
  Touches: `crates/terminalai-core/src/launch.rs`, `environment.rs`, `web/src/main.js`, `web/src/contextPressure.js`, `crates/terminalai-core/tests/launch_golden.rs`
  Acceptance: the launcher can set a compaction threshold per session, it reaches the previewed argv for both agents, and the row shows it beside the occupancy so a session near its own threshold is visible before it compacts. An agent with no equivalent is refused rather than silently launched without it, per `LaunchError::Unsupported`.
  Complexity: S

### P3

## Tell the agent when the operator is using a screen reader

Blocked 2026-08-07: the flag does not exist in the Claude Code installed on this machine, so the
mapping cannot be verified against the binary it would run.

`--ax-screen-reader` and `CLAUDE_AX_SCREEN_READER` are documented from **v2.1.181**. This machine
has **2.1.170** — `claude --version` reports it, and `claude --help` lists no `--ax` flag and no
screen-reader option at all. Claude Code rejects unknown flags, so mapping it now would ship a
launcher that produces an argv this machine's agent refuses, and no test here could catch it: the
argv goldens assert what we emit, not what the agent accepts.

This is the same trap the plan-mode item named from the other side — *the flag being documented is
not proof this build accepts it* — and the answer is the same: verify against the installed binary.
Codex has no equivalent at all (`codex --help` shows none), so that half is a refusal rather than a
mapping regardless of version.

Unblocks by upgrading Claude Code to 2.1.181 or later. Deliberately not done as part of an
autonomous drain: Claude Code is the tool the operator is actively running, and upgrading it
mid-session changes the environment the session itself depends on.

When unblocked, the acceptance is unchanged: turning on the app's screen-reader mode launches new
sessions with the agent's equivalent where the agent has one, says plainly that it cannot change
sessions already running, and refuses rather than silently launching an agent that has no equivalent
as if it had one — per `LaunchError::Unsupported`.

Original item:

- [ ] P3 — Tell the agent when the operator is using a screen reader
  Why: the app has an explicit opt-in screen-reader mode for its own terminal, and the agent whose output fills that terminal has a matching mode that is never turned on, so the accessible surface stops at the renderer.
  Evidence: the focused terminal toolbar's screen-reader opt-in is described in `README.md:132-133`. Claude Code exposes `--ax-screen-reader`, "render screen-reader friendly output; flat text without decorations (v2.1.181+)", and the environment variable `CLAUDE_AX_SCREEN_READER` (https://code.claude.com/docs/en/cli-reference, /settings). Neither is reachable: the flag is not in `launch.rs`'s table and the variable is not in `safe_environment_keys()`.
  Touches: `crates/terminalai-core/src/launch.rs`, `environment.rs`, `web/src/main.js`
  Acceptance: turning on the app's screen-reader mode launches new sessions with the agent's equivalent where the agent has one, and says plainly that it cannot change sessions already running. An agent with no equivalent is not silently launched as if it had one. Pairs with the launcher-flag passthrough item above; file the flag there if that item lands first.
  Complexity: S

## Verify Windows toast delivery on an interactive desktop

Blocked 2026-08-03: requires an interactive desktop session, which the standing visual-isolation
rule forbids this agent from using. The app is launched onto a private desktop, where the shell that
renders notifications is not running, so a toast raised there cannot appear by construction; and
PowerShell on this machine cannot load the WinRT projection to raise one independently.

The decisions are unit-tested — which states toast, the text of each, dedup by session and status,
and click-to-focus through the in-process `on_activated` handler. Delivery is not. What closes it is
one operator run on the interactive desktop with the Start-Menu shortcut present: confirm the toast
appears, names the session, and that clicking it focuses that row.

## Usability acceptance test with a person who has not seen the tool

The implementation now includes a task-oriented first-run checklist, a read-only safe demo that
covers every fleet status without starting an agent, the row → focused-terminal explainer whose
state list is generated from the same table the rows are drawn from, plain-language descriptions
for every status and the dwell timer, and packaged WebView2 proof that the demo never attaches a
daemon output channel. The deterministic suite also checks that every icon-only control — static
or built at runtime — carries a tooltip and an accessible name.

What cannot be done here is the item's own acceptance criterion: *someone unfamiliar with the tool
can launch a session, read its state, and answer an attention request unaided*. That needs a person
who has not seen it, and this agent is the worst possible judge of whether its own explanation is
clear. Watch one run, note where they hesitate, and reopen the item against what they got stuck on
rather than against a checklist. The safe demo and checklist reduce the cost of that run; they do
not substitute for it.

## ACP transport as an alternative to the pty

Gated on a product decision this agent cannot make, not on effort. The item's own 2026-08-02
research concluded it should be built *only when a third agent family is added*: ACP v1 is stable,
but neither Claude Code nor Codex speaks it natively, so today it would mean adopting two
third-party adapters (`@zed-industries/claude-code-acp`, `agentclientprotocol/codex-acp`) to reach
agents the pty already drives directly — new dependencies and a second transport for no capability
the fleet lacks. Implementing it now would contradict the recorded finding rather than act on it.

Unblocks when a third agent family is on the roadmap; the adapters are then the cheapest way in.

## R-06 · P1 — Session hibernation and rehydration

Blocked pending live validation that `claude --resume <id>` restores enough context for
transparent hibernation. The implementation requires a real Claude session and operator-visible
judgment about whether the resumed context is equivalent; no safe local inference can settle that
question.

## R-56 · P2 — A UI test and screenshot path that actually works

Blocked 2026-08-03 by the required Windows visual-isolation display, not by the application test
code. The signed virtual display driver was healthy but Windows attached no isolated screen: the
approved `visual-isolation.ps1 ensure` failed closed before and after `remove` plus a verified
driver reinstall. The embedded WebDriver provider then timed out at the first `element` command,
and a direct native EdgeDriver probe independently failed with `DevToolsActivePort file doesn't
exist` after launching the app on the verified private desktop. Resume only after `ensure` returns
the exact fourth virtual display; do not use a physical monitor or an interactive desktop.

Update 2026-08-03 (later session): `ensure` succeeds again — `\\.\DISPLAY5` 1920x1080 at (5360,0)
attached, and `launch` placed the release app on it with placement proof. The display blocker is
gone; the WebDriver/EdgeDriver failures above are unretested since and are now the open question.

## P3 unaudited-surface pass residuals — 2026-08-03

The catch-all testing row was audited on Windows 11 26100 and removed from `ROADMAP.md`.

- **VT/grid:** closed for in-process and recorded-stream coverage. `cargo test -p terminalai-core
  --all-features grid` passed 14 tests, including the arbitrary-byte property test, and
  `cargo test -p terminalai-core --all-features --test grid_ref` passed the recorded Claude/Codex
  streams across parser chunks.
- **MCP and runtime capabilities:** closed for the boundary and parser layers. The MCP integration
  suite passed 18 tests, and the capability suite passed 4 tests covering model catalogs, Claude
  init capabilities, runtime feature output, and unknown-value warnings. A live vendor-binary probe
  is part of the external-agent limitation below.
- **Opt-in `codex-app-server`:** the feature-gated daemon suite passed 3 tests for typed requests,
  initialization, and the frame limit. An end-to-end handshake with a real Codex app-server was not
  run: the installed `codex.ps1` launcher is present, but there is no authenticated disposable
  agent session available for this pass.
- **Non-Windows paths:** static `cfg` and target-dependency review covered the Unix and Windows
  branches, and the Windows workspace suites passed. macOS/Linux compilation and execution cannot
  be performed from this Windows host; keep a cross-host CI or host run as the closure for those
  branches.
- **NSIS/MSI payloads:** a fresh `cargo tauri build --ci --no-sign --bundles nsis,msi` succeeded.
  The generated WiX source contains the main executable and both declared sidecars, and the MSI
  contains their component names. The compressed NSIS payload was not extracted because no local
  archive extractor is installed. The install-and-launch gate remains under R-56: running the
  existing verifier as-is would start its installer on the interactive desktop before its later
  isolated application launch.
- **Real load and memory claims:** the frontend contract suite still covers the declared 28px row,
  measured-window documentation, and 29-row target, but no fresh browser measurement or live-agent
  RSS/load run was performed in this pass. The 2026-08-02/2026-08-03 measurements remain historical
  evidence rather than a new live-agent load result. A deterministic injected-domain gate landed
  2026-08-08: `scripts/verify-fleet-stress.ps1` runs 30 synthetic sessions and 1,920 hook events,
  enforces startup/latency/RSS/working-set budgets, and proves bounded scrollback, subscriber
  backlog, store round-trip and malformed-store rejection. It is evidence for the registry and
  daemon paths, not a substitute for an operator-owned authenticated-agent measurement.
- **Agent-level round trip:** no authenticated Claude or Codex session exists on this machine, so
  launching a real prompt, hook, capability probe, or app-server conversation would be speculative.
  Close this residual with an operator-owned authenticated run using the isolated UI path.

## P0 live Claude transcript filename validation — 2026-08-04

The caller-supplied session binding is implemented and covered by the full Windows workspace
suite. New Claude launches now receive a generated UUID through `--session-id`; the tail checks
`%USERPROFILE%\\.claude\\projects\\<slug>\\<uuid>.jsonl` directly and records whether it used the
explicit binding or the compatibility heuristic.

Live vendor validation remains open because this machine has no authenticated Claude session. The
documented flag and filename convention were not exercised against a real Claude process, so this
does not claim that external acceptance. Close it with an operator-owned isolated launch that
confirms the generated id creates the expected JSONL path and that two same-folder sessions each
remain attached to their own transcript.

## P0 managed hook policy live-device residual — 2026-08-04

Preflight now evaluates the local Windows policy chain: the HKLM policy value, the system
`managed-settings.json` plus sorted drop-ins, and the HKCU fallback. Unit tests cover the effective
merge and the exact non-fixable `blocked` result without changing this machine's administrator
policy. A server-managed or policy-helper result delivered only inside a Claude session is not
observable before that session starts, so this local preflight cannot claim to detect those remote
sources. An operator-owned run with `disableAllHooks: true` in an approved managed source can
confirm the full banner against a real Claude installation.

## Publish a winget manifest

Blocked 2026-08-04: the manifest lives in `microsoft/winget-pkgs`, not here, and submission needs a
published GitHub Release whose asset URLs and SHA256 are already fixed. Neither exists to point at
yet, and opening a PR against a third-party repository is an outward-facing action for the operator.

The research behind it is settled and does not need redoing: signing is required only for MSIX, so an
unsigned NSIS/EXE passes validation — `wez.wezterm`, `yt-dlp.yt-dlp` and `Neovim.Neovim` are live
proof. Schema 1.12.0. Declare `Silent: /S` for NSIS and `/quiet` for MSI, `UpgradeBehavior`, and
`AppsAndFeaturesEntries` carrying the MSI `UpgradeCode`.

Update 2026-08-06: the release-side blocker is gone. v0.10.0 is published at
https://github.com/SysAdminDoc/TerminalAI/releases/tag/v0.10.0 with both installers attached, and
the NSIS sidecar-lock fix it warned about shipped in v0.7.0 — `scripts/verify-installer.ps1` ran
against the exact uploaded artifact, including the upgrade-over-a-running-daemon path, before
publishing. What remains is only the outward-facing half: opening a PR against the third-party
`microsoft/winget-pkgs` repository, which is the operator's action to take.

Update 2026-08-08: the local release side is now repeatable. A tagged build runs
`scripts/prepare-release-assets.ps1`, which copies the exact versioned NSIS/MSI files, emits
`SHA256SUMS` and `release-manifest.json`, and generates a schema-validated three-file Winget
manifest with the MSI ProductCode and UpgradeCode read from the built database. The remaining
blocker is still the operator-owned PR against `microsoft/winget-pkgs` after a tagged release is
published.

## Publish checksums and a detached signature with each release

Blocked 2026-08-04: the signature needs a private key the operator holds, which this agent must
never generate or store. `SHA256SUMS` alone is a partial answer and would advertise a verification
the signature half does not back.

Update 2026-08-06: the key is now the only blocker. A published release for the step to attach to
exists as of v0.10.0.

Update 2026-08-08: the tagged workflow now publishes unsigned `SHA256SUMS` as a byte-integrity
aid and states the no-signing policy in the release manifest. It intentionally does not generate a
detached signature; the private key/public-key step remains blocked on operator-owned key material.

What closes it: the operator generates a minisign keypair once (`minisign -G`, or `tauri signer
generate`), keeps the secret key out of the repository, and publishes the public key in `README.md`.
The release step then emits `SHA256SUMS` plus `SHA256SUMS.minisig`. Minisign is Ed25519 with no CA
and no PKI, so this is not code signing and does not touch the no-signing rule — and it changes
nothing about SmartScreen, which reads signatures and per-hash download volume only.

## Live validation of a redirected agent config directory — 2026-08-06

The mechanism shipped: a launch may set `CLAUDE_CONFIG_DIR` / `CODEX_HOME` and name parent variables
to inherit, refusing any name that is malformed, reserved or unset. It is covered by the workspace
suite and driven headlessly by `terminalai-probe start --agent-home <dir> --env-passthrough <NAME>`.

What cannot be settled here is whether a redirected config directory fragments the agent's own
state. `~/.claude.json` holds the OAuth session, the MCP server list *and* per-project state
together, so pointing two sessions at two directories may also split what `--resume` can see and
which MCP servers a session gets. This machine has no authenticated agent, so the question cannot be
answered by running it.

What closes it: one operator-owned launch with `--agent-home` pointing at a fresh directory,
confirming that the session signs in independently, that `--resume` inside it lists only that
directory's sessions, and that MCP servers configured in the default directory are absent rather
than silently inherited. If they are inherited, the README's "two directories are two accounts"
needs qualifying to name exactly what is and is not separated.

## Corroborating cost from OpenTelemetry — 2026-08-07

The premise holds and is worth restating: Anthropic documents the transcript entry format as
internal and version-changing, so the entire cost path in this tool rests on a contract that is
explicitly not promised, while a sanctioned metrics surface (`claude_code.cost.usage`,
`claude_code.token.usage`, with `query_source` attribution) exists.

What cannot be settled here is the part the acceptance turns on. Claude Code exposes no CLI flag for
this at all — `claude --help` on 2.1.170 mentions neither OpenTelemetry nor telemetry — so it is
configured by environment and by an exporter endpoint the operator owns. Two things follow that this
machine cannot supply:

1. **Whether export is permitted on a subscription plan**, which is an account question rather than
   a code one. Building a preference for a channel that turns out to be unavailable would leave the
   fleet with a fallback path it always takes and a tooltip claiming a source it never has.
2. **An OTel collector to export to.** The acceptance requires the fleet to prefer the endpoint when
   one is configured and to log a disagreement beyond a threshold — neither of which can be
   exercised, let alone verified, without a running receiver.

What closes it: confirmation that telemetry export is allowed on the operator's plan, plus an
endpoint to point at. Then the work is real and bounded — read the two metrics, prefer them when
present, keep transcript arithmetic as the fallback, and log rather than silently resolve a
disagreement. Note for whoever picks it up: both sources are client-side estimates, so neither is
the provider's accounting and the wording must not imply otherwise — the same rule the quota-window
breakdown shipped under in v0.20.0.

## Whether a teammate's hooks arrive as the lead's

Blocked 2026-08-08 on evidence this machine cannot produce. Agent teams is opt-in and experimental
(`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`) and is not enabled here — `~/.claude/teams` does not
exist — so there is no team to observe. Settling it needs a real team run under a supervised
session, which is an operator decision about their own Claude Code, not a drain's.

What closes it: one supervised session running a team, with the daemon's hook log read while a
teammate finishes. Then either the teammate's `Notification` and `SubagentStop` arrive carrying the
lead's `hook_token` — in which case the arms added on 2026-08-08 need something that distinguishes
them — or they do not, and that is recorded in the ingest module docs so the question stops being
reopened.

Local protection landed 2026-08-08: `HookAttribution` now returns `Matched`, `Unknown`, or
`Ambiguous`; an authenticated fallback with multiple candidates is refused and counted rather than
mutating the first row. The replay tests
`an_inherited_team_token_is_refused_when_two_rows_are_candidates` and
`a_native_session_id_wins_over_an_inherited_token_candidate` cover the lead/teammate interleaving
that can be proven without a real team. The vendor-specific question remains blocked because only a
real team run can reveal whether a distinct instance identifier is present in the hook payload.

Original item:

- [ ] P2 — Find out whether a teammate's hooks arrive as the lead's
  Why: a teammate launched by a team lead inherits the lead's environment, including the hook token the daemon matches on — so a `Notification` or `SubagentStop` from a separate agent instance may be landing on the lead's row and moving a status that is not about it. Nothing today can tell the two apart, and the answer decides whether the notification arms added on 2026-08-08 need scoping.
  Evidence: hook matching requires `entry.session.hook_token` (`crates/terminalai-core/src/registry/ingest.rs`), which is placed in the session's environment at launch; `https://code.claude.com/docs/en/agent-teams` says teammates are separate Claude Code instances started by the lead. `agent_completed` currently sets the lead's row to `AwaitingInput` by name — the same mapping the substring match produced before — and if teammate completions arrive on that token, a lead still working would be reported as waiting.
  Touches: `crates/terminalai-core/src/registry/ingest.rs`, `crates/terminalai-core/src/hooks.rs`
  Acceptance: a real team run is observed and the finding recorded either way. If teammate hooks do arrive on the lead's token, they carry something that distinguishes them and the notification arms use it; if they do not, that is stated in the ingest module docs so the question is not reopened.
  Complexity: S

## Make the hooks preflight prove itself rather than read a file

Blocked 2026-08-08 on the same upgrade as the two version-gated items above: `claude --init-only`
does not exist on 2.1.170, and it is the only way to make a hook actually fire without starting a
conversation. Cannot start until the operator upgrades.

Local delivery accounting landed 2026-08-08. The daemon now counts valid hook events by agent and
exposes matched, unmatched, and ambiguous totals over the control pipe; preflight reports
“installed, not yet proven” until an event is observed and “installed and firing” only afterwards.
The counters reset with a daemon restart, so stale configuration cannot become proof. The remaining
blocker is still the operator-owned vendor trigger: the installed Claude 2.1.170 does not expose
`--init-only`, so this machine cannot produce the real probe event required by the acceptance.

Original item:

- [ ] P3 — Make the hooks preflight prove itself rather than read a file
  Why: the check reports "installed" from the settings file and from managed policy, which is a claim about configuration, not evidence that a hook fires and reaches the daemon. Cannot start until the Claude Code upgrade item above lands — `--init-only` does not exist on 2.1.170.
  Evidence: the `hooks` check (`crates/terminalai-app/src/main.rs:1495`, `:1600-1650`) inspects installed state and blocking policy only. `claude --init-only` — "run Setup and SessionStart hooks, then exit without starting a conversation" (https://code.claude.com/docs/en/cli-reference) — would make the hook actually fire; it is absent from `claude --help` on 2.1.170, so this depends on the 2.1.226 upgrade item above.
  Touches: `crates/terminalai-app/src/main.rs`, `crates/terminalai-daemon/src/http_hooks.rs`
  Acceptance: the preflight check reports "installed and firing" only after the daemon observed a hook from a real `--init-only` run, distinguishes that from "installed, not yet proven", and never blocks startup on the probe failing.
  Complexity: M
