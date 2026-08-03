mod preset;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::{io, io::Read};

use preset::{Preset, PresetStore};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Emitter, Manager, State};
use terminalai_core::agent::Agent;
use terminalai_core::launch::LaunchSpec;
use terminalai_core::{
    parse_hook, AdmissionSnapshot, AgentCapabilities, HookTransport, LogEntry, RegistryEvent,
    ReviewItem, Session, SessionId, SessionStatus, MAX_LOG_ENTRIES,
};
use terminalai_daemon::{
    DaemonClient, HookEndpoint, IpcError, Request, Response, PROTOCOL_VERSION,
};

struct AppState {
    client: Mutex<Option<DaemonClient>>,
    presets: PresetStore,
    output_channels: Arc<Mutex<HashMap<SessionId, Channel<InvokeResponseBody>>>>,
}

#[derive(Debug, Serialize)]
struct FleetSnapshot {
    sessions: Vec<Session>,
    focused: Option<SessionId>,
    admission: AdmissionSnapshot,
    store_quarantine: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReviewSnapshot {
    entries: Vec<ReviewItem>,
}

#[derive(Debug, Serialize)]
struct LaunchReceipt {
    id: SessionId,
    queued: bool,
}

#[derive(Debug, Serialize, Clone)]
struct PreflightCheck {
    id: String,
    label: String,
    state: String,
    detected: String,
    detail: Option<String>,
    can_fix: bool,
}

#[derive(Debug, Serialize, Clone)]
struct PreflightReport {
    checks: Vec<PreflightCheck>,
}

const APP_USER_MODEL_ID: &str = "com.sysadmindoc.terminalai";
const PREFLIGHT_DAEMON_TIMEOUT: Duration = Duration::from_millis(500);

#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn daemon_response(client: &DaemonClient, request: Request) -> Result<Response, String> {
    client.call(request).map_err(|error| error.to_string())
}

fn daemon_client(state: &State<'_, AppState>) -> Result<DaemonClient, String> {
    state
        .client
        .lock()
        .map_err(|_| "daemon client state is poisoned".to_string())?
        .clone()
        .ok_or_else(|| "daemon is unavailable; run preflight checks and retry".to_string())
}

fn require_ok(response: Response) -> Result<(), String> {
    match response {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
}

#[tauri::command]
fn fleet_snapshot(state: State<'_, AppState>) -> Result<FleetSnapshot, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Snapshot)? {
        Response::Snapshot {
            sessions,
            focused,
            admission,
            store_quarantine,
        } => Ok(FleetSnapshot {
            sessions,
            focused,
            admission,
            store_quarantine,
        }),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected snapshot response: {other:?}")),
    }
}

#[tauri::command]
fn review_snapshot(state: State<'_, AppState>) -> Result<ReviewSnapshot, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::ReviewSnapshot)? {
        Response::ReviewSnapshot { entries } => Ok(ReviewSnapshot { entries }),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected review response: {other:?}")),
    }
}

/// Sessions running outside this supervisor. Read-only by construction: the
/// response carries no handle the UI could act on.
#[tauri::command]
fn external_sessions(
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::ExternalSession>, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::ExternalSessions)? {
        Response::ExternalSessions { sessions } => Ok(sessions),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected external-session response: {other:?}")),
    }
}

#[tauri::command]
fn mark_reviewed(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::MarkReviewed { id })?)
}

#[tauri::command]
fn preview_launch(
    spec: LaunchSpec,
    configured_path: Option<PathBuf>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let client = daemon_client(&state)?;
    match daemon_response(
        &client,
        Request::Preview {
            spec: Box::new(spec),
            configured_path,
        },
    )? {
        Response::Preview { command } => Ok(command),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected preview response: {other:?}")),
    }
}

#[tauri::command]
fn resolve_agent(
    agent: Agent,
    configured_path: Option<PathBuf>,
    state: State<'_, AppState>,
) -> Result<Response, String> {
    let client = daemon_client(&state)?;
    match daemon_response(
        &client,
        Request::Resolve {
            agent,
            configured_path,
        },
    )? {
        Response::Error { message } => Err(message),
        response => Ok(response),
    }
}

#[tauri::command]
fn agent_capabilities(
    agent: Agent,
    configured_path: Option<PathBuf>,
    state: State<'_, AppState>,
) -> Result<AgentCapabilities, String> {
    let client = daemon_client(&state)?;
    match daemon_response(
        &client,
        Request::Capabilities {
            agent,
            configured_path,
        },
    )? {
        Response::Capabilities { capabilities } => Ok(capabilities),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected capabilities response: {other:?}")),
    }
}

#[tauri::command]
fn launch_session(
    spec: LaunchSpec,
    configured_path: Option<PathBuf>,
    state: State<'_, AppState>,
) -> Result<LaunchReceipt, String> {
    let client = daemon_client(&state)?;
    match daemon_response(
        &client,
        Request::Launch {
            spec: Box::new(spec),
            configured_path,
        },
    )? {
        Response::Launched { id, queued } => Ok(LaunchReceipt { id, queued }),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected launch response: {other:?}")),
    }
}

#[tauri::command]
fn write_session(id: SessionId, data: String, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::Write { id, data })?)
}

#[tauri::command]
fn resize_session(
    id: SessionId,
    rows: u16,
    cols: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(
        &client,
        Request::Resize {
            id,
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        },
    )?)
}

#[tauri::command]
fn kill_session(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::Kill { id })?)
}

#[tauri::command]
fn focus_session(id: Option<SessionId>, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::Focus { id })?)
}

#[tauri::command]
fn mark_read(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::MarkRead { id })?)
}

#[tauri::command]
fn toggle_pin(id: SessionId, state: State<'_, AppState>) -> Result<bool, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::TogglePin { id })? {
        Response::PinChanged { pinned } => Ok(pinned),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected pin response: {other:?}")),
    }
}

fn register_output_channel(
    id: SessionId,
    channel: Channel<InvokeResponseBody>,
    channels: &Arc<Mutex<HashMap<SessionId, Channel<InvokeResponseBody>>>>,
) -> Result<(), String> {
    let mut channels = channels
        .lock()
        .map_err(|_| "output channel registry is poisoned".to_string())?;
    channels.retain(|session_id, _| session_id == &id);
    channels.insert(id, channel);
    Ok(())
}

fn send_raw(channel: &Channel<InvokeResponseBody>, data: Vec<u8>) -> Result<(), String> {
    channel
        .send(InvokeResponseBody::Raw(data))
        .map_err(|error| format!("send terminal bytes: {error}"))
}

#[tauri::command]
fn subscribe_output(
    id: SessionId,
    channel: Channel<InvokeResponseBody>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    register_output_channel(id, channel, &state.output_channels)
}

#[tauri::command]
fn stream_scrollback(
    id: SessionId,
    channel: Channel<InvokeResponseBody>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Scrollback { id })? {
        Response::Scrollback { data } => send_raw(&channel, data),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected scrollback response: {other:?}")),
    }
}

#[tauri::command]
fn attach_session_output(
    id: SessionId,
    channel: Channel<InvokeResponseBody>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    register_output_channel(id.clone(), channel.clone(), &state.output_channels)?;
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Reattach { id })? {
        Response::Reattached { data } => send_raw(&channel, data),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected reattach response: {other:?}")),
    }
}

#[tauri::command]
fn revive_session(id: SessionId, state: State<'_, AppState>) -> Result<SessionId, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Revive { id })? {
        Response::Revived { id } => Ok(id),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected revive response: {other:?}")),
    }
}

#[tauri::command]
fn archive_session(id: SessionId, state: State<'_, AppState>) -> Result<SessionId, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Archive { id })? {
        Response::Archived { id } => Ok(id),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected archive response: {other:?}")),
    }
}

#[tauri::command]
fn list_presets(state: State<'_, AppState>) -> Result<Vec<Preset>, String> {
    state.presets.list()
}

#[tauri::command]
fn save_preset(preset: Preset, state: State<'_, AppState>) -> Result<(), String> {
    state.presets.save(preset)
}

#[tauri::command]
fn delete_preset(name: String, state: State<'_, AppState>) -> Result<bool, String> {
    state.presets.delete(&name)
}

#[tauri::command]
fn pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Choose project folder")
        .pick_folder()
        .map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
fn pick_extra_dirs() -> Vec<String> {
    rfd::FileDialog::new()
        .set_title("Choose extra writable folders")
        .pick_folders()
        .unwrap_or_default()
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// Open an OSC 8 hyperlink emitted by a session.
///
/// The URI comes from agent output, which is untrusted: a session that renders
/// attacker-controlled text can emit any hyperlink it likes. Only the three
/// schemes a terminal link plausibly needs are honoured, so `file:`, `vbscript:`
/// and any registered custom protocol handler are refused rather than handed to
/// `ShellExecute`. The refusal is reported, never swallowed.
/// Decide whether a session-supplied URI may be opened. Kept separate from the
/// command so the rules can be tested without launching a browser.
fn validate_external_url(url: &str) -> Result<&str, String> {
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
fn open_external_url(url: String) -> Result<String, String> {
    let target = validate_external_url(&url)?;
    open::that_detached(target).map_err(|error| format!("could not open link: {error}"))?;
    Ok(target.to_owned())
}

#[tauri::command]
fn preflight_report() -> PreflightReport {
    PreflightReport {
        checks: vec![
            preflight_agent(Agent::Claude),
            preflight_agent(Agent::Codex),
            preflight_hooks(),
            preflight_daemon(),
            preflight_shortcut(),
        ],
    }
}

#[tauri::command]
fn preflight_fix(
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
    let endpoint = DaemonClient::connect_with_timeout(PREFLIGHT_DAEMON_TIMEOUT)
        .ok()
        .and_then(|client| client.hook_endpoint().ok());
    let mut detected = Vec::new();
    let mut details = Vec::new();
    let mut healthy = true;
    for agent in [Agent::Claude, Agent::Codex] {
        let path = terminalai_core::hook_config_path(agent, &home, codex_home.as_deref());
        let transport = hook_transport(agent, &executable, endpoint.as_ref());
        match terminalai_core::hook_status_at_with_transport(agent, &path, &transport) {
            Ok(status) => {
                let state = if status.disabled {
                    "disabled"
                } else if status.stale {
                    "stale"
                } else if status.installed {
                    "installed"
                } else {
                    "missing"
                };
                if state != "installed" {
                    healthy = false;
                }
                detected.push(format!("{}: {state}", agent.command_name()));
                details.push(format!("{} → {}", agent.command_name(), path.display()));
            }
            Err(error) => {
                healthy = false;
                detected.push(format!("{}: error", agent.command_name()));
                details.push(format!("{}: {error}", agent.command_name()));
            }
        }
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
    agent: Agent,
    executable: &std::path::Path,
    endpoint: Option<&HookEndpoint>,
) -> HookTransport {
    if agent == Agent::Claude {
        if let Some(endpoint) = endpoint {
            return HookTransport::Http {
                url: endpoint.url_for(agent),
                host: endpoint.host.clone(),
                bearer_token: endpoint.bearer_token.clone(),
            };
        }
    }
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

fn connect_or_start_daemon() -> Result<DaemonClient, String> {
    match DaemonClient::connect() {
        Ok(client) => return Ok(client),
        Err(error @ IpcError::VersionMismatch { .. }) => {
            return Err(error.to_string());
        }
        Err(IpcError::Remote(message)) if message.starts_with("incompatible control protocol:") => {
            return Err(message);
        }
        Err(_) => {}
    }

    let executable = std::env::current_exe()
        .map_err(|error| format!("find TerminalAI executable: {error}"))?
        .with_file_name(if cfg!(windows) {
            "terminalai-daemon.exe"
        } else {
            "terminalai-daemon"
        });
    if !executable.is_file() {
        return Err(format!(
            "daemon is not running and the sibling executable is missing: {}",
            executable.display()
        ));
    }
    let mut command = Command::new(&executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .spawn()
        .map_err(|error| format!("start daemon: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_error = String::from("daemon did not accept a connection");
    while Instant::now() < deadline {
        match DaemonClient::connect() {
            Ok(client) => return Ok(client),
            Err(error @ IpcError::VersionMismatch { .. }) => return Err(error.to_string()),
            Err(IpcError::Remote(message))
                if message.starts_with("incompatible control protocol:") =>
            {
                return Err(message);
            }
            Err(error) => last_error = error.to_string(),
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "{last_error}; daemon executable: {}",
        executable.display()
    ))
}

fn install_daemon_client(
    app: &tauri::AppHandle,
    state: &State<'_, AppState>,
    client: DaemonClient,
) -> Result<(), String> {
    client
        .subscribe()
        .map_err(|error| format!("subscribe to daemon protocol v{PROTOCOL_VERSION}: {error}"))?;
    bridge_daemon_events(app, &client, state.output_channels.clone());
    *state
        .client
        .lock()
        .map_err(|_| "daemon client state is poisoned".to_string())? = Some(client);
    Ok(())
}

const OUTPUT_BATCH_INTERVAL: Duration = Duration::from_millis(12);
const LOG_BATCH_INTERVAL: Duration = Duration::from_millis(100);

fn bridge_daemon_events(
    app: &tauri::AppHandle,
    client: &DaemonClient,
    output_channels: Arc<Mutex<HashMap<SessionId, Channel<InvokeResponseBody>>>>,
) {
    let initial_waiting = client
        .call_with_timeout(Request::Snapshot, Duration::from_secs(2))
        .ok()
        .and_then(|response| match response {
            Response::Snapshot { sessions, .. } => Some(waiting_sessions(&sessions)),
            _ => None,
        })
        .unwrap_or_default();
    let receiver = client.events();
    let app = app.clone();
    let _ = thread::Builder::new()
        .name("terminalai-ui-events".into())
        .spawn(move || {
            let mut waiting = initial_waiting;
            let mut rendered_waiting = None;
            update_taskbar_waiting_count(&app, waiting.len());
            let mut pending = HashMap::<SessionId, Vec<u8>>::new();
            let mut pending_logs = VecDeque::<LogEntry>::new();
            let mut next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
            let mut next_log_flush = Instant::now() + LOG_BATCH_INTERVAL;
            loop {
                let now = Instant::now();
                let timeout = next_output_flush
                    .saturating_duration_since(now)
                    .min(next_log_flush.saturating_duration_since(now));
                let received = receiver
                    .lock()
                    .ok()
                    .map(|events| events.recv_timeout(timeout));
                match received {
                    Some(Ok(RegistryEvent::Output { id, data })) => {
                        pending.entry(id).or_default().extend(data);
                    }
                    Some(Ok(RegistryEvent::Log { entry })) => {
                        pending_logs.push_back(entry);
                        while pending_logs.len() > MAX_LOG_ENTRIES {
                            let _ = pending_logs.pop_front();
                        }
                    }
                    Some(Ok(event)) => {
                        match &event {
                            RegistryEvent::SessionUpdated { session } => {
                                if is_waiting_session(session) {
                                    waiting.insert(session.id.clone());
                                } else {
                                    waiting.remove(&session.id);
                                }
                            }
                            RegistryEvent::SessionRemoved { id } => {
                                waiting.remove(id);
                            }
                            _ => {}
                        }
                        if rendered_waiting != Some(waiting.len()) {
                            update_taskbar_waiting_count(&app, waiting.len());
                            rendered_waiting = Some(waiting.len());
                        }
                        flush_output_batches(&mut pending, &output_channels);
                        next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                        if app.emit("terminalai:event", event).is_err() {
                            break;
                        }
                    }
                    Some(Err(RecvTimeoutError::Timeout)) => {
                        flush_output_batches(&mut pending, &output_channels);
                        next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                    }
                    Some(Err(RecvTimeoutError::Disconnected)) | None => {
                        flush_output_batches(&mut pending, &output_channels);
                        let _ = flush_log_batches(&mut pending_logs, &app);
                        break;
                    }
                }
                if Instant::now() >= next_log_flush {
                    if !flush_log_batches(&mut pending_logs, &app) {
                        break;
                    }
                    next_log_flush = Instant::now() + LOG_BATCH_INTERVAL;
                }
            }
        });
}

fn is_waiting_session(session: &Session) -> bool {
    matches!(
        session.status,
        SessionStatus::NeedsApproval | SessionStatus::AwaitingInput | SessionStatus::NeedsYou
    )
}

fn waiting_sessions(sessions: &[Session]) -> HashSet<SessionId> {
    sessions
        .iter()
        .filter(|session| is_waiting_session(session))
        .map(|session| session.id.clone())
        .collect()
}

#[cfg(target_os = "windows")]
fn taskbar_badge_image(count: usize) -> tauri::image::Image<'static> {
    const SIZE: usize = 32;
    const DIGITS: [[u8; 7]; 10] = [
        [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b11100,
        ],
    ];
    let text = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    let scale = if text.len() == 1 { 3usize } else { 2usize };
    let glyph_width = 5 * scale;
    let spacing = scale;
    let total_width = text.len() * glyph_width + text.len().saturating_sub(1) * spacing;
    let start_x = (SIZE.saturating_sub(total_width)) / 2;
    let start_y = (SIZE - 7 * scale) / 2;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as isize - 16;
            let dy = y as isize - 16;
            if dx * dx + dy * dy <= 15 * 15 {
                let offset = (y * SIZE + x) * 4;
                rgba[offset..offset + 4].copy_from_slice(&[210, 76, 74, 255]);
            }
        }
    }
    let mut cursor_x = start_x;
    for character in text.chars() {
        let glyph = if let Some(digit) = character.to_digit(10) {
            DIGITS[digit as usize]
        } else {
            [
                0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
            ]
        };
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = cursor_x + col * scale + dx;
                        let y = start_y + row * scale + dy;
                        if x < SIZE && y < SIZE {
                            let offset = (y * SIZE + x) * 4;
                            rgba[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
            }
        }
        cursor_x += glyph_width + spacing;
    }
    tauri::image::Image::new_owned(rgba, SIZE as u32, SIZE as u32)
}

fn update_taskbar_waiting_count(app: &tauri::AppHandle, count: usize) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    #[cfg(target_os = "windows")]
    let result = window.set_overlay_icon((count != 0).then(|| taskbar_badge_image(count)));
    #[cfg(not(target_os = "windows"))]
    let result = window.set_badge_count((count != 0).then_some(count as i64));
    if let Err(error) = result {
        eprintln!("could not update taskbar waiting count ({count}): {error}");
    }
}

fn flush_log_batches(pending: &mut VecDeque<LogEntry>, app: &tauri::AppHandle) -> bool {
    if pending.is_empty() {
        return true;
    }
    let batch: Vec<_> = pending.drain(..).collect();
    app.emit("terminalai:logs", batch).is_ok()
}

fn flush_output_batches(
    pending: &mut HashMap<SessionId, Vec<u8>>,
    output_channels: &Arc<Mutex<HashMap<SessionId, Channel<InvokeResponseBody>>>>,
) {
    let batches = std::mem::take(pending);
    for (id, data) in batches {
        let channel = output_channels
            .lock()
            .ok()
            .and_then(|channels| channels.get(&id).cloned());
        let Some(channel) = channel else {
            continue;
        };
        if channel.send(InvokeResponseBody::Raw(data)).is_err() {
            if let Ok(mut channels) = output_channels.lock() {
                channels.remove(&id);
            }
        }
    }
}

fn run_app() -> Result<(), String> {
    #[cfg(feature = "wdio")]
    {
        // tauri-driver passes this through msedgedriver, but EdgeDriver may not
        // preserve it when it launches the application binary. Set it before
        // Tauri creates the first WebView2 environment so external and embedded
        // WDIO providers have the same automation contract.
        std::env::set_var("TAURI_AUTOMATION", "true");
        std::env::set_var("TAURI_WEBVIEW_AUTOMATION", "true");
    }
    #[cfg_attr(not(feature = "wdio"), allow(unused_mut))]
    let mut builder = tauri::Builder::default();
    #[cfg(feature = "wdio")]
    {
        builder = builder.plugin(tauri_plugin_wdio::init());
    }
    #[cfg(feature = "wdio-embedded")]
    {
        builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    }
    builder
        .invoke_handler(tauri::generate_handler![
            app_version,
            fleet_snapshot,
            review_snapshot,
            external_sessions,
            mark_reviewed,
            preview_launch,
            resolve_agent,
            agent_capabilities,
            launch_session,
            write_session,
            resize_session,
            kill_session,
            focus_session,
            mark_read,
            toggle_pin,
            subscribe_output,
            stream_scrollback,
            attach_session_output,
            revive_session,
            archive_session,
            list_presets,
            save_preset,
            delete_preset,
            pick_folder,
            pick_extra_dirs,
            preflight_report,
            preflight_fix,
            open_external_url
        ])
        .setup(|app| {
            let client = if cfg!(feature = "wdio") {
                None
            } else {
                match connect_or_start_daemon() {
                    Ok(client) => match client.subscribe() {
                        Ok(()) => Some(client),
                        Err(error) => {
                            eprintln!("TerminalAI daemon subscription unavailable: {error}");
                            None
                        }
                    },
                    Err(error) => {
                        eprintln!("TerminalAI daemon unavailable: {error}");
                        None
                    }
                }
            };
            let presets = PresetStore::load_default().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            let output_channels = Arc::new(Mutex::new(HashMap::new()));
            if let Some(client) = client.as_ref() {
                bridge_daemon_events(app.handle(), client, output_channels.clone());
            }
            app.manage(AppState {
                client: Mutex::new(client),
                presets,
                output_channels,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|error| error.to_string())
}

fn is_hook_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == "hook")
}

/// Deliver an agent hook without initializing Tauri/WebView2.
///
/// Hook commands are deliberately fail-open: an unavailable desktop daemon
/// must never stall or fail the user's agent command. Claude's async hook
/// support normally keeps this off the agent's critical path; the short
/// timeout also bounds Codex's synchronous command hook.
fn run_hook_cli(args: &[String]) -> i32 {
    let Some(agent_arg) = args.first() else {
        eprintln!("usage: terminalai hook <claude|codex>");
        return 0;
    };
    let agent = match agent_arg.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => {
            eprintln!("ignoring hook for unknown agent: {other}");
            return 0;
        }
    };
    let mut input = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut input) {
        eprintln!("ignoring hook input read failure: {error}");
        return 0;
    }
    let event = match parse_hook(agent, &input) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("ignoring malformed hook input: {error}");
            return 0;
        }
    };
    let timeout = Duration::from_millis(750);
    let client = match DaemonClient::connect_with_timeout(timeout) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("ignoring hook because TerminalAI is unavailable: {error}");
            return 0;
        }
    };
    match client.call_with_timeout(Request::Hook { event }, timeout) {
        Ok(Response::Hook { .. }) | Ok(Response::Ok) => {}
        Ok(other) => eprintln!("ignoring unexpected hook response: {other:?}"),
        Err(error) => eprintln!("ignoring hook delivery failure: {error}"),
    }
    0
}

fn main() {
    let _logging = terminalai_daemon::init_logging();
    terminalai_daemon::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if is_hook_invocation(&args) {
        std::process::exit(run_hook_cli(&args[1..]));
    }
    if let Err(error) = run_app() {
        eprintln!("TerminalAI: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_schemes_are_opened_from_session_output() {
        // Every one of these is a URI a session could emit inside an OSC 8
        // sequence, and each would be handed to ShellExecute without the check.
        for refused in [
            "file:///C:/Windows/System32/calc.exe",
            "vbscript:msgbox(1)",
            "javascript:alert(1)",
            "ms-msdt:/id",
            "search-ms:query=x",
            r"\\attacker\share\payload.exe",
            "C:\\Windows\\System32\\calc.exe",
            "http://",
            "",
        ] {
            let error = validate_external_url(refused)
                .expect_err(&format!("{refused:?} must not be opened"));
            assert!(error.starts_with("refused"), "{refused:?} gave {error}");
        }
    }

    #[test]
    fn a_link_carrying_control_characters_is_refused() {
        let error = validate_external_url("https://example.com/\u{1b}]0;pwned\u{7}")
            .expect_err("control characters must be refused");
        assert!(error.contains("control characters"), "{error}");
    }

    #[test]
    fn ordinary_links_are_opened() {
        // Uppercase spellings are legal URI syntax; refusing them would send an
        // operator chasing a link that looks identical to a working one.
        for allowed in [
            "https://example.com/path?q=1#frag",
            "http://localhost:3000/",
            "HTTPS://Example.COM",
            "mailto:someone@example.com",
            "  https://example.com/padded  ",
        ] {
            let target = validate_external_url(allowed)
                .unwrap_or_else(|error| panic!("{allowed:?} should open: {error}"));
            assert_eq!(target, allowed.trim());
        }
    }

    #[test]
    fn protocol_version_is_pinned_for_the_shell() {
        assert_eq!(PROTOCOL_VERSION, 2);
    }

    #[test]
    fn hook_invocation_bypasses_the_gui_shell() {
        assert!(is_hook_invocation(&["hook".into(), "claude".into()]));
        assert!(!is_hook_invocation(&["--help".into()]));
        assert!(!is_hook_invocation(&[]));
    }
}
