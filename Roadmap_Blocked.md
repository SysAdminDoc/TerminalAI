# Blocked roadmap items

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

The implementation shipped in v0.6.0: first-run guidance that points at registering a root, an
in-app explainer of the row → focused-terminal model whose state list is generated from the same
table the rows are drawn from, a plain-language description for every status and for the dwell
timer, and a test that every icon-only control — static or built at runtime — carries a tooltip
and an accessible name.

What cannot be done here is the item's own acceptance criterion: *someone unfamiliar with the tool
can launch a session, read its state, and answer an attention request unaided*. That needs a person
who has not seen it, and this agent is the worst possible judge of whether its own explanation is
clear. Watch one run, note where they hesitate, and reopen the item against what they got stuck on
rather than against a checklist.

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
  evidence rather than a new load result.
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

## Publish checksums and a detached signature with each release

Blocked 2026-08-04: the signature needs a private key the operator holds, which this agent must
never generate or store. `SHA256SUMS` alone is a partial answer and would advertise a verification
the signature half does not back.

Update 2026-08-06: the key is now the only blocker. A published release for the step to attach to
exists as of v0.10.0.

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
