//! Operator preflight checks and repair commands.

use super::*;

/// Open an OSC 8 hyperlink emitted by a session.
///
/// The URI comes from agent output, which is untrusted: a session that renders
/// attacker-controlled text can emit any hyperlink it likes. Only the three
/// schemes a terminal link plausibly needs are honoured, so `file:`, `vbscript:`
/// and any registered custom protocol handler are refused rather than handed to
/// `ShellExecute`. The refusal is reported, never swallowed.
/// Decide whether a session-supplied URI may be opened. Kept separate from the
/// command so the rules can be tested without launching a browser.
pub(crate) fn validate_external_url(url: &str) -> Result<&str, String> {
    const ALLOWED: [&str; 3] = ["http://", "https://", "mailto:"];
    let trimmed = url.trim();
    // Control characters would survive into whatever handles the URI.
    if trimmed.chars().any(char::is_control) {
        return Err("refused to open a link containing control characters".to_owned());
    }
    let lowered = trimmed.to_ascii_lowercase();
    let scheme_ok = ALLOWED
        .iter()
        .any(|scheme| lowered.starts_with(scheme) && trimmed.len() > scheme.len());
    if !scheme_ok {
        return Err(format!(
            "refused to open {trimmed:?}: only http, https and mailto links are opened"
        ));
    }
    Ok(trimmed)
}

#[tauri::command]
pub(crate) fn open_external_url(url: String) -> Result<String, String> {
    let target = validate_external_url(&url)?;
    open::that_detached(target).map_err(|error| format!("could not open link: {error}"))?;
    Ok(target.to_owned())
}

#[tauri::command]
pub(crate) async fn preflight_report() -> Result<PreflightReport, String> {
    run_blocking("preflight_report", || {
        Ok(PreflightReport {
            checks: vec![
                preflight_agent(Agent::Claude),
                preflight_agent(Agent::Codex),
                preflight_hooks(),
                preflight_daemon(),
                preflight_shortcut(),
            ],
        })
    })
    .await
}

#[tauri::command]
pub(crate) fn preflight_fix(
    kind: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match kind.as_str() {
        "hooks" => install_preflight_hooks(),
        "daemon" => {
            let client = connect_or_start_daemon()?;
            install_daemon_client(&app, &state, client)
        }
        "shortcut" => create_start_menu_shortcut(),
        other => Err(format!("no automatic fix is available for {other}")),
    }
}

fn preflight_agent(agent: Agent) -> PreflightCheck {
    let label = format!("{} CLI", agent.label());
    match terminalai_core::agent::resolve(agent, None) {
        Ok(binary) => match agent_version(&binary.path) {
            Ok(version) => PreflightCheck {
                id: agent.command_name().into(),
                label,
                state: "ok".into(),
                detected: format!("{version} · {}", binary.path.display()),
                detail: Some(format!("Resolved via {:?}", binary.origin)),
                can_fix: false,
            },
            Err(error) => PreflightCheck {
                id: agent.command_name().into(),
                label,
                state: "error".into(),
                detected: binary.path.display().to_string(),
                detail: Some(error),
                can_fix: false,
            },
        },
        Err(error) => PreflightCheck {
            id: agent.command_name().into(),
            label,
            state: "error".into(),
            detected: "Not found".into(),
            detail: Some(error.to_string()),
            can_fix: false,
        },
    }
}

fn agent_version(path: &std::path::Path) -> Result<String, String> {
    let mut command = Command::new(path);
    command.arg("--version");
    terminalai_core::environment::configure_command_environment(&mut command, &[]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run --version: {error}"))?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    let version = text.lines().next().unwrap_or_default().trim();
    if !output.status.success() {
        return Err(format!("--version exited unsuccessfully: {version}"));
    }
    if version.is_empty() {
        return Err("--version returned no text".into());
    }
    Ok(version.chars().take(160).collect())
}

fn preflight_hooks() -> PreflightCheck {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("terminalai"));
    let managed_policy = match terminalai_core::managed_hook_policy() {
        Ok(policy) => policy,
        Err(error) => {
            return PreflightCheck {
                id: "hooks".into(),
                label: "Managed hooks".into(),
                state: "error".into(),
                detected: "Managed policy unreadable".into(),
                detail: Some(error.to_string()),
                can_fix: false,
            };
        }
    };
    let daemon = DaemonClient::connect_with_timeout(PREFLIGHT_DAEMON_TIMEOUT).ok();
    let endpoint = daemon
        .as_ref()
        .and_then(|client| client.hook_endpoint().ok());
    let delivery = daemon
        .as_ref()
        .and_then(|client| client.hook_delivery_status().ok());
    let mut detected = Vec::new();
    let mut details = Vec::new();
    let mut healthy = true;
    let mut claude_installed = false;
    for agent in [Agent::Claude, Agent::Codex] {
        let path = terminalai_core::hook_config_path(agent, &home, codex_home.as_deref());
        let transport = hook_transport(agent, &executable, endpoint.as_ref());
        match terminalai_core::hook_status_at_with_transport(agent, &path, &transport) {
            Ok(status) => {
                if agent == Agent::Claude {
                    claude_installed = status.installed;
                }
                let observed = delivery
                    .as_ref()
                    .is_some_and(|delivery| delivery.for_agent(agent).observed);
                let state =
                    preflight_hook_state(status.installed, status.disabled, status.stale, observed);
                if state != "installed and firing" {
                    healthy = false;
                }
                detected.push(format!("{}: {state}", agent.command_name()));
                let evidence = delivery
                    .as_ref()
                    .map(|delivery| {
                        let status = delivery.for_agent(agent);
                        format!(
                            "; observed events: {} (matched {}, ambiguous {}, unmatched {})",
                            status.observed_events,
                            status.matched_events,
                            status.ambiguous_events,
                            status.unmatched_events
                        )
                    })
                    .unwrap_or_else(|| "; daemon delivery proof unavailable".into());
                details.push(format!(
                    "{} → {}{}",
                    agent.command_name(),
                    path.display(),
                    evidence
                ));
            }
            Err(error) => {
                healthy = false;
                detected.push(format!("{}: error", agent.command_name()));
                details.push(format!("{}: {error}", agent.command_name()));
            }
        }
    }
    if let Some(policy) = managed_policy.filter(|policy| policy.blocks_user_hooks()) {
        return blocked_hook_preflight(&policy, claude_installed, &details.join(" · "));
    }
    PreflightCheck {
        id: "hooks".into(),
        label: "Managed hooks".into(),
        state: if healthy { "ok" } else { "warn" }.into(),
        detected: detected.join(" · "),
        detail: Some(details.join(" · ")),
        can_fix: true,
    }
}

pub(crate) fn preflight_hook_state(
    installed: bool,
    disabled: bool,
    stale: bool,
    observed: bool,
) -> &'static str {
    if disabled {
        "disabled"
    } else if stale {
        "stale"
    } else if !installed {
        "missing"
    } else if observed {
        "installed and firing"
    } else {
        "installed, not yet proven"
    }
}

pub(crate) fn blocked_hook_preflight(
    policy: &terminalai_core::ManagedHookPolicy,
    claude_installed: bool,
    details: &str,
) -> PreflightCheck {
    let sources = policy.sources.join(", ");
    let settings = policy.blocking_settings().join(", ");
    let detected = if claude_installed {
        format!("hooks installed but disabled by policy at {sources}")
    } else {
        format!("hooks cannot fire: policy disables them at {sources}")
    };
    PreflightCheck {
        id: "hooks".into(),
        label: "Managed hooks".into(),
        state: "blocked".into(),
        detected,
        detail: Some(format!(
            "Claude managed policy sets {settings}. TerminalAI cannot override administrator policy; remove or change it outside this app. {details}"
        )),
        can_fix: false,
    }
}

fn install_preflight_hooks() -> Result<(), String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let endpoint = connect_or_start_daemon()
        .ok()
        .and_then(|client| client.hook_endpoint().ok());
    for agent in [Agent::Claude, Agent::Codex] {
        let path = terminalai_core::hook_config_path(agent, &home, codex_home.as_deref());
        let transport = hook_transport(agent, &executable, endpoint.as_ref());
        let result = terminalai_core::install_hooks_at_with_transport(agent, &path, &transport);
        if let Err(error) = result {
            if matches!(transport, HookTransport::Http { .. }) {
                terminalai_core::install_hooks_at(agent, &path, &executable).map_err(
                    |fallback| {
                        format!(
                            "install {} hooks ({error}); command fallback also failed: {fallback}",
                            agent.command_name()
                        )
                    },
                )?;
            } else {
                return Err(format!("install {} hooks: {error}", agent.command_name()));
            }
        }
    }
    Ok(())
}

fn hook_transport(
    _agent: Agent,
    executable: &std::path::Path,
    _endpoint: Option<&HookEndpoint>,
) -> HookTransport {
    // Claude's HTTP settings are global, while hook authentication is
    // intentionally per session. The managed command inherits the token from
    // the supervised agent environment, so two sessions in one repository
    // cannot address each other's rows. The daemon HTTP endpoint remains
    // available for explicit callers that can provide both secrets.
    HookTransport::Command {
        executable: executable.to_path_buf(),
    }
}

fn preflight_daemon() -> PreflightCheck {
    match DaemonClient::connect_with_timeout(PREFLIGHT_DAEMON_TIMEOUT) {
        Ok(_) => PreflightCheck {
            id: "daemon".into(),
            label: "Daemon reachable".into(),
            state: "ok".into(),
            detected: format!("Protocol v{PROTOCOL_VERSION}"),
            detail: Some("Named-pipe control plane accepted a handshake".into()),
            can_fix: false,
        },
        Err(error) => PreflightCheck {
            id: "daemon".into(),
            label: "Daemon reachable".into(),
            state: "error".into(),
            detected: "Unreachable".into(),
            detail: Some(error.to_string()),
            can_fix: true,
        },
    }
}

fn start_menu_shortcut_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        dirs::data_dir().map(|data| {
            data.join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("TerminalAI.lnk")
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn preflight_shortcut() -> PreflightCheck {
    let Some(path) = start_menu_shortcut_path() else {
        return PreflightCheck {
            id: "shortcut".into(),
            label: "Start-Menu shortcut".into(),
            state: "unsupported".into(),
            detected: "Not applicable on this platform".into(),
            detail: None,
            can_fix: false,
        };
    };
    if path.is_file() {
        #[cfg(windows)]
        return preflight_shortcut_file(&path);
        #[cfg(not(windows))]
        unreachable!("a Start-Menu path is only available on Windows");
    } else {
        PreflightCheck {
            id: "shortcut".into(),
            label: "Start-Menu shortcut".into(),
            state: "warn".into(),
            detected: format!("Missing · System.AppUserModel.ID={APP_USER_MODEL_ID}"),
            detail: Some(path.display().to_string()),
            can_fix: true,
        }
    }
}

#[cfg(windows)]
fn preflight_shortcut_file(path: &std::path::Path) -> PreflightCheck {
    let base = |state: &str, detected: String, detail: String, can_fix: bool| -> PreflightCheck {
        PreflightCheck {
            id: "shortcut".into(),
            label: "Start-Menu shortcut".into(),
            state: state.into(),
            detected,
            detail: Some(detail),
            can_fix,
        }
    };
    match read_shortcut_app_user_model_id(path) {
        Ok(id) if id == APP_USER_MODEL_ID => base(
            "ok",
            format!("Installed · System.AppUserModel.ID={id}"),
            path.display().to_string(),
            false,
        ),
        Ok(id) if id.is_empty() => base(
            "warn",
            "Installed · System.AppUserModel.ID is missing".into(),
            path.display().to_string(),
            true,
        ),
        Ok(id) => base(
            "warn",
            format!("Installed · System.AppUserModel.ID={id}"),
            format!("Expected {APP_USER_MODEL_ID}; {}", path.display()),
            true,
        ),
        Err(error) => base(
            "warn",
            "Installed · AppUserModel ID could not be read".into(),
            format!("{}: {error}", path.display()),
            true,
        ),
    }
}

#[cfg(windows)]
const SHORTCUT_PROPERTY_SCRIPT: &str = r#"
$source = @'
using System;
using System.Runtime.InteropServices;

[StructLayout(LayoutKind.Sequential, Pack = 4)]
public struct PROPERTYKEY {
    public Guid fmtid;
    public uint pid;
    public PROPERTYKEY(Guid fmtid, uint pid) { this.fmtid = fmtid; this.pid = pid; }
}

[StructLayout(LayoutKind.Explicit)]
public struct PROPVARIANT {
    [FieldOffset(0)] public ushort vt;
    [FieldOffset(2)] public ushort wReserved1;
    [FieldOffset(4)] public ushort wReserved2;
    [FieldOffset(6)] public ushort wReserved3;
    [FieldOffset(8)] public IntPtr pointerValue;
}

[ComImport, Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IPropertyStore {
    void GetCount(out uint count);
    void GetAt(uint index, out PROPERTYKEY key);
    void GetValue(ref PROPERTYKEY key, out PROPVARIANT value);
    void SetValue(ref PROPERTYKEY key, ref PROPVARIANT value);
    void Commit();
}

[ComImport, Guid("0000010b-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IPersistFile {
    void GetClassID(out Guid classId);
    int IsDirty();
    void Load([MarshalAs(UnmanagedType.LPWStr)] string fileName, uint mode);
    void Save([MarshalAs(UnmanagedType.LPWStr)] string fileName, [MarshalAs(UnmanagedType.Bool)] bool remember);
    void SaveCompleted([MarshalAs(UnmanagedType.LPWStr)] string fileName);
    void GetCurFile([MarshalAs(UnmanagedType.LPWStr)] out string fileName);
}

public static class PropVariantNativeMethods {
    [DllImport("ole32.dll")]
    public static extern int PropVariantClear(ref PROPVARIANT value);
}
'@
Add-Type -TypeDefinition $source

if ($env:TERMINALAI_SHORTCUT_MODE -eq 'write') {
    $wsh = New-Object -ComObject WScript.Shell
    $wshLink = $wsh.CreateShortcut($env:TERMINALAI_SHORTCUT_PATH)
    $wshLink.TargetPath = $env:TERMINALAI_TARGET
    $wshLink.WorkingDirectory = Split-Path $env:TERMINALAI_TARGET
    $wshLink.Description = 'TerminalAI'
    $wshLink.Save()
    [Runtime.InteropServices.Marshal]::ReleaseComObject($wshLink) | Out-Null
    [Runtime.InteropServices.Marshal]::ReleaseComObject($wsh) | Out-Null
}

$shellLink = [Activator]::CreateInstance([Type]::GetTypeFromCLSID([Guid]'00021401-0000-0000-C000-000000000046'))
try {
    $persist = [IPersistFile]$shellLink
    $persist.Load($env:TERMINALAI_SHORTCUT_PATH, 2)
    $propertyStore = [IPropertyStore]$shellLink
    $key = [PROPERTYKEY]::new([Guid]'9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3', 5)
    if ($env:TERMINALAI_SHORTCUT_MODE -eq 'write') {
        $value = New-Object PROPVARIANT
        $value.vt = 31
        $value.pointerValue = [Runtime.InteropServices.Marshal]::StringToCoTaskMemUni($env:TERMINALAI_APP_ID)
        try {
            $propertyStore.SetValue([ref]$key, [ref]$value)
            $propertyStore.Commit()
            $persist.Save($env:TERMINALAI_SHORTCUT_PATH, $true)
        } finally {
            [PropVariantNativeMethods]::PropVariantClear([ref]$value) | Out-Null
        }
    } else {
        $value = New-Object PROPVARIANT
        try {
            $propertyStore.GetValue([ref]$key, [ref]$value)
            if ($value.vt -eq 31 -and $value.pointerValue -ne [IntPtr]::Zero) {
                [Runtime.InteropServices.Marshal]::PtrToStringUni($value.pointerValue)
            }
        } finally {
            [PropVariantNativeMethods]::PropVariantClear([ref]$value) | Out-Null
        }
    }
} finally {
    if ($propertyStore) { [Runtime.InteropServices.Marshal]::ReleaseComObject($propertyStore) | Out-Null }
    if ($persist) { [Runtime.InteropServices.Marshal]::ReleaseComObject($persist) | Out-Null }
    if ($shellLink) { [Runtime.InteropServices.Marshal]::ReleaseComObject($shellLink) | Out-Null }
}
"#;

#[cfg(windows)]
fn shortcut_property_command(
    path: &std::path::Path,
    mode: &str,
    target: Option<&std::path::Path>,
) -> Result<String, String> {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        SHORTCUT_PROPERTY_SCRIPT,
    ]);
    terminalai_core::environment::configure_command_environment(&mut command, &[]);
    command.env("TERMINALAI_SHORTCUT_PATH", path);
    command.env("TERMINALAI_SHORTCUT_MODE", mode);
    command.env("TERMINALAI_APP_ID", APP_USER_MODEL_ID);
    if let Some(target) = target {
        command.env("TERMINALAI_TARGET", target);
    }
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if error.is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            error
        })
    }
}

#[cfg(windows)]
fn read_shortcut_app_user_model_id(path: &std::path::Path) -> Result<String, String> {
    shortcut_property_command(path, "read", None)
}

fn create_start_menu_shortcut() -> Result<(), String> {
    #[cfg(not(windows))]
    {
        return Err("Start-Menu shortcuts are only supported on Windows".into());
    }
    #[cfg(windows)]
    {
        let path = start_menu_shortcut_path().ok_or("could not locate the Start-Menu folder")?;
        let target = std::env::current_exe().map_err(|error| error.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        shortcut_property_command(&path, "write", Some(&target)).map(|_| ())
    }
}
