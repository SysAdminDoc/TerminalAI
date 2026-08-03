mod preset;

use std::collections::HashMap;
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
    parse_hook, AdmissionSnapshot, RegistryEvent, ReviewItem, Session, SessionId,
};
use terminalai_daemon::{DaemonClient, Request, Response, PROTOCOL_VERSION};

struct AppState {
    client: DaemonClient,
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

#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn daemon_response(client: &DaemonClient, request: Request) -> Result<Response, String> {
    client.call(request).map_err(|error| error.to_string())
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
    match daemon_response(&state.client, Request::Snapshot)? {
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
    match daemon_response(&state.client, Request::ReviewSnapshot)? {
        Response::ReviewSnapshot { entries } => Ok(ReviewSnapshot { entries }),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected review response: {other:?}")),
    }
}

#[tauri::command]
fn mark_reviewed(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    require_ok(daemon_response(
        &state.client,
        Request::MarkReviewed { id },
    )?)
}

#[tauri::command]
fn preview_launch(
    spec: LaunchSpec,
    configured_path: Option<PathBuf>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    match daemon_response(
        &state.client,
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
    match daemon_response(
        &state.client,
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
fn launch_session(
    spec: LaunchSpec,
    configured_path: Option<PathBuf>,
    state: State<'_, AppState>,
) -> Result<LaunchReceipt, String> {
    match daemon_response(
        &state.client,
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
    require_ok(daemon_response(&state.client, Request::Write { id, data })?)
}

#[tauri::command]
fn resize_session(
    id: SessionId,
    rows: u16,
    cols: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    require_ok(daemon_response(
        &state.client,
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
    require_ok(daemon_response(&state.client, Request::Kill { id })?)
}

#[tauri::command]
fn focus_session(id: Option<SessionId>, state: State<'_, AppState>) -> Result<(), String> {
    require_ok(daemon_response(&state.client, Request::Focus { id })?)
}

#[tauri::command]
fn mark_read(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    require_ok(daemon_response(&state.client, Request::MarkRead { id })?)
}

#[tauri::command]
fn toggle_pin(id: SessionId, state: State<'_, AppState>) -> Result<bool, String> {
    match daemon_response(&state.client, Request::TogglePin { id })? {
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
    match daemon_response(&state.client, Request::Scrollback { id })? {
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
    match daemon_response(&state.client, Request::Reattach { id })? {
        Response::Reattached { data } => send_raw(&channel, data),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected reattach response: {other:?}")),
    }
}

#[tauri::command]
fn revive_session(id: SessionId, state: State<'_, AppState>) -> Result<SessionId, String> {
    match daemon_response(&state.client, Request::Revive { id })? {
        Response::Revived { id } => Ok(id),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected revive response: {other:?}")),
    }
}

#[tauri::command]
fn archive_session(id: SessionId, state: State<'_, AppState>) -> Result<SessionId, String> {
    match daemon_response(&state.client, Request::Archive { id })? {
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

fn connect_or_start_daemon() -> Result<DaemonClient, String> {
    if let Ok(client) = DaemonClient::connect() {
        return Ok(client);
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
            Err(error) => last_error = error.to_string(),
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "{last_error}; daemon executable: {}",
        executable.display()
    ))
}

const OUTPUT_BATCH_INTERVAL: Duration = Duration::from_millis(12);

fn bridge_daemon_events(
    app: &tauri::AppHandle,
    client: &DaemonClient,
    output_channels: Arc<Mutex<HashMap<SessionId, Channel<InvokeResponseBody>>>>,
) {
    let receiver = client.events();
    let app = app.clone();
    let _ = thread::Builder::new()
        .name("terminalai-ui-events".into())
        .spawn(move || {
            let mut pending = HashMap::<SessionId, Vec<u8>>::new();
            let mut next_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
            loop {
                let timeout = next_flush.saturating_duration_since(Instant::now());
                let received = receiver
                    .lock()
                    .ok()
                    .map(|events| events.recv_timeout(timeout));
                match received {
                    Some(Ok(RegistryEvent::Output { id, data })) => {
                        pending.entry(id).or_default().extend(data);
                    }
                    Some(Ok(event)) => {
                        flush_output_batches(&mut pending, &output_channels);
                        next_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                        if app.emit("terminalai:event", event).is_err() {
                            break;
                        }
                    }
                    Some(Err(RecvTimeoutError::Timeout)) => {
                        flush_output_batches(&mut pending, &output_channels);
                        next_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                    }
                    Some(Err(RecvTimeoutError::Disconnected)) | None => {
                        flush_output_batches(&mut pending, &output_channels);
                        break;
                    }
                }
            }
        });
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
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            app_version,
            fleet_snapshot,
            review_snapshot,
            mark_reviewed,
            preview_launch,
            resolve_agent,
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
            pick_extra_dirs
        ])
        .setup(|app| {
            let client = connect_or_start_daemon().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            if let Err(error) = client.subscribe() {
                return Err(Box::new(std::io::Error::other(format!(
                    "subscribe to daemon protocol v{PROTOCOL_VERSION}: {error}"
                ))) as Box<dyn std::error::Error>);
            }
            let presets = PresetStore::load_default().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            let output_channels = Arc::new(Mutex::new(HashMap::new()));
            bridge_daemon_events(app.handle(), &client, output_channels.clone());
            app.manage(AppState {
                client,
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
    let client =
        match DaemonClient::connect_named_with_timeout(terminalai_daemon::PIPE_NAME, timeout) {
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
