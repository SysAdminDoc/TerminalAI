#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod preset;
mod projects;
mod toast;
mod work;

use terminalai_core::work_queue::{EntryState, WorkQueue};

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
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
    parse_hook_in, AdmissionSnapshot, AgentCapabilities, HookTransport, LogEntry, RegistryEvent,
    ReviewItem, Session, SessionId, SessionStatus, MAX_LOG_ENTRIES,
};
use terminalai_daemon::{
    DaemonClient, HookEndpoint, IpcError, Request, Response, PROTOCOL_VERSION,
};

type OutputChannels = Arc<Mutex<HashMap<SessionId, Arc<OutputRoute>>>>;

struct OutputRoute {
    channel: Channel<InvokeResponseBody>,
    state: Mutex<OutputRouteState>,
}

struct OutputRouteState {
    replaying: bool,
    pending: Vec<u8>,
}

impl OutputRoute {
    fn new(channel: Channel<InvokeResponseBody>, replaying: bool) -> Self {
        Self {
            channel,
            state: Mutex::new(OutputRouteState {
                replaying,
                pending: Vec::new(),
            }),
        }
    }

    fn queue(&self, data: Vec<u8>) -> Result<(), String> {
        self.state
            .lock()
            .map_err(|_| "output route is poisoned".to_string())?
            .pending
            .extend(data);
        Ok(())
    }

    fn flush(&self) -> Result<(), String> {
        let data = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "output route is poisoned".to_string())?;
            if state.replaying || state.pending.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut state.pending)
        };
        self.channel
            .send(InvokeResponseBody::Raw(data))
            .map_err(|error| format!("send terminal bytes: {error}"))
    }

    fn complete_replay(&self, replay: Vec<u8>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "output route is poisoned".to_string())?;
        let pending = std::mem::take(&mut state.pending);
        let pending_start = replay_overlap(&replay, &pending);
        self.channel
            .send(InvokeResponseBody::Raw(replay))
            .map_err(|error| format!("send terminal replay: {error}"))?;
        if pending_start < pending.len() {
            self.channel
                .send(InvokeResponseBody::Raw(pending[pending_start..].to_vec()))
                .map_err(|error| format!("send terminal output after replay: {error}"))?;
        }
        state.replaying = false;
        Ok(())
    }
}

/// Return the number of buffered bytes already covered by the replay.
///
/// Normally the buffered stream starts with a suffix of the ring. Searching
/// for the longest matching suffix also handles a very long RPC where the
/// ring has already dropped the oldest part of the buffered stream: those
/// bytes must still be discarded because the replay is the pane's reset point.
fn replay_overlap(replay: &[u8], pending: &[u8]) -> usize {
    let mut best_length = 0;
    let mut best_end = 0;
    for start in 0..pending.len() {
        let max_length = replay.len().min(pending.len() - start);
        for length in (1..=max_length).rev() {
            if replay[replay.len() - length..] == pending[start..start + length] {
                let end = start + length;
                if length > best_length || (length == best_length && end < best_end) {
                    best_length = length;
                    best_end = end;
                }
                break;
            }
        }
    }
    best_end
}

struct AppState {
    client: Mutex<Option<DaemonClient>>,
    presets: PresetStore,
    project_roots: projects::ProjectRoots,
    prompts: work::PromptLibrary,
    work_run_store: work::WorkRunStore,
    output_channels: OutputChannels,
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

async fn run_blocking<T, F>(label: &'static str, task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| format!("{label} background task failed: {error}"))?
}

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
async fn external_sessions(
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::ExternalSession>, String> {
    let client = daemon_client(&state)?;
    run_blocking("external_sessions", move || {
        match daemon_response(&client, Request::ExternalSessions)? {
            Response::ExternalSessions { sessions } => Ok(sessions),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected external-session response: {other:?}")),
        }
    })
    .await
}

/// Read the daemon-wide admission policy for the settings dialog.
#[tauri::command]
fn admission_config(
    state: State<'_, AppState>,
) -> Result<terminalai_daemon::AdmissionSettings, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::AdmissionConfig)? {
        Response::Admission { admission } => Ok(admission),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected admission response: {other:?}")),
    }
}

/// Replace the daemon-wide admission policy without restarting it.
#[tauri::command]
fn set_admission(
    settings: terminalai_daemon::AdmissionSettings,
    state: State<'_, AppState>,
) -> Result<terminalai_daemon::AdmissionSettings, String> {
    let client = daemon_client(&state)?;
    let request = Request::SetAdmission {
        max_live_sessions: settings.max_live_sessions,
        default_budget_usd: settings.default_budget_usd,
        spend_ceiling_usd: settings.spend_ceiling_usd,
        spend_window_hours: Some(settings.spend_window_hours),
        memory_budget_mb: settings.memory_budget_mb,
        session_memory_cap_mb: settings.session_memory_cap_mb,
        max_processes_per_session: settings.max_processes_per_session,
    };
    // Reported by the daemon, never sent back: the boot environment is not the
    // dialog's to rewrite.
    match daemon_response(&client, request)? {
        Response::Admission { admission } => Ok(admission),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected admission response: {other:?}")),
    }
}

#[tauri::command]
fn mark_reviewed(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::MarkReviewed { id })?)
}

/// Land a session's uncommitted work into a target repository, or report the
/// specific reason it was refused.
///
/// The daemon serialises these, so this command blocks while another landing is
/// in flight — that wait is the feature, not an oversight.
#[tauri::command]
async fn land_session(
    request: terminalai_core::land::LandRequest,
    state: State<'_, AppState>,
) -> Result<terminalai_core::land::LandOutcome, String> {
    let client = daemon_client(&state)?;
    run_blocking("land_session", move || {
        match daemon_response(
            &client,
            Request::Land {
                request: Box::new(request),
            },
        )? {
            Response::Land { outcome } => Ok(outcome),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected land response: {other:?}")),
        }
    })
    .await
}

/// The parsed terminal state for a pinned pane.
///
/// A pinned session keeps a live grid in Rust but no browser renderer — that is
/// what lets the fleet hold ~29 rows. The split view reads this instead of
/// instantiating a second xterm.
#[tauri::command]
fn grid_snapshot(
    id: SessionId,
    state: State<'_, AppState>,
) -> Result<terminalai_core::TerminalGridSnapshot, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::GridSnapshot { id })? {
        Response::GridSnapshot { grid } => Ok(grid),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected grid response: {other:?}")),
    }
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
    // Keep the raw terminal stream intact: the daemon uses its line-ending
    // boundary to distinguish composition from an explicit send.
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
    channels: &OutputChannels,
    replaying: bool,
) -> Result<Arc<OutputRoute>, String> {
    let mut channels = channels
        .lock()
        .map_err(|_| "output channel registry is poisoned".to_string())?;
    let route = Arc::new(OutputRoute::new(channel, replaying));
    channels.retain(|session_id, _| session_id == &id);
    channels.insert(id, route.clone());
    Ok(route)
}

fn remove_output_route(id: &SessionId, route: &Arc<OutputRoute>, channels: &OutputChannels) {
    if let Ok(mut channels) = channels.lock() {
        if channels
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, route))
        {
            channels.remove(id);
        }
    }
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
    register_output_channel(id, channel, &state.output_channels, false).map(|_| ())
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

/// Prompts waiting their turn on one session.
#[tauri::command]
fn queued_prompts(
    id: SessionId,
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::queue::QueuedPrompt>, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::QueuedPrompts { id })? {
        Response::QueuedPrompts { prompts } => Ok(prompts),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected queue response: {other:?}")),
    }
}

#[tauri::command]
fn enqueue_prompt(id: SessionId, text: String, state: State<'_, AppState>) -> Result<u64, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::EnqueuePrompt { id, text })? {
        Response::Enqueued { prompt } => Ok(prompt),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected queue response: {other:?}")),
    }
}

#[tauri::command]
fn edit_queued_prompt(
    id: SessionId,
    prompt: u64,
    text: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    expect_ok(&state, Request::EditQueuedPrompt { id, prompt, text })
}

#[tauri::command]
fn remove_queued_prompt(
    id: SessionId,
    prompt: u64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    expect_ok(&state, Request::RemoveQueuedPrompt { id, prompt })
}

#[tauri::command]
fn reorder_queued_prompt(
    id: SessionId,
    prompt: u64,
    to: usize,
    state: State<'_, AppState>,
) -> Result<(), String> {
    expect_ok(&state, Request::ReorderQueuedPrompt { id, prompt, to })
}

#[tauri::command]
fn pause_queue(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    expect_ok(&state, Request::PauseQueue { id })
}

#[tauri::command]
fn resume_queue(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    expect_ok(&state, Request::ResumeQueue { id })
}

/// Send a request whose only successful answer is `Ok`.
fn expect_ok(state: &State<'_, AppState>, request: Request) -> Result<(), String> {
    let client = daemon_client(state)?;
    match daemon_response(&client, request)? {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Send one prompt to several sessions, returning what happened to each.
///
/// The per-session result is returned to the caller rather than collapsed into
/// a status, so the UI can say "sent to 5 of 9" instead of "sent" — a broadcast
/// that reports only success is one the operator has to verify by hand.
#[tauri::command]
fn broadcast_prompt(
    ids: Vec<SessionId>,
    text: String,
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::BroadcastResult>, String> {
    let client = daemon_client(&state)?;
    // The same bracketed-paste framing a single reply uses. Without it a
    // multi-line prompt is submitted a line at a time, so the agent acts on the
    // first fragment.
    let data = format!("\u{1b}[200~{text}\u{1b}[201~\r");
    match daemon_response(&client, Request::Broadcast { ids, data })? {
        Response::Broadcast { results } => Ok(results),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected broadcast response: {other:?}")),
    }
}

/// Output the in-memory ring has already dropped, read from the disk tier.
///
/// Streamed on a channel like the ring is, because it is the same kind of
/// payload and a Tauri command's return value is JSON — a serialized byte array
/// costs several times its own length.
#[tauri::command]
fn stream_scrollback_history(
    id: SessionId,
    max_bytes: u64,
    channel: Channel<InvokeResponseBody>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::ScrollbackHistory { id, max_bytes })? {
        Response::ScrollbackHistory { data } => send_raw(&channel, data),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected history response: {other:?}")),
    }
}

#[tauri::command]
fn attach_session_output(
    id: SessionId,
    channel: Channel<InvokeResponseBody>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let route = register_output_channel(id.clone(), channel, &state.output_channels, true)?;
    let client = match daemon_client(&state) {
        Ok(client) => client,
        Err(error) => {
            remove_output_route(&id, &route, &state.output_channels);
            return Err(error);
        }
    };
    let result = match daemon_response(&client, Request::Reattach { id: id.clone() }) {
        Ok(Response::Reattached { data }) => route.complete_replay(data),
        Ok(Response::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected reattach response: {other:?}")),
        Err(error) => Err(error),
    };
    if result.is_err() {
        remove_output_route(&id, &route, &state.output_channels);
    }
    result
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

/// Launch templates the chosen repository declares about itself.
///
/// Read on demand from the folder in the launcher rather than cached, because
/// the file is versioned with the repository: pulling a branch that changes it
/// should change what the launcher offers, without restarting anything.
///
/// A malformed file is an error the operator sees, not an empty list. Silently
/// ignoring it launches with them believing the repository's own defaults were
/// applied.
#[tauri::command]
fn list_templates(cwd: PathBuf) -> Result<Vec<terminalai_core::template::Template>, String> {
    terminalai_core::template::load(&cwd).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_preset(preset: Preset, state: State<'_, AppState>) -> Result<(), String> {
    state.presets.save(preset)
}

/// The stored prompt library.
#[tauri::command]
fn list_stored_prompts(state: State<'_, AppState>) -> Result<Vec<work::StoredPrompt>, String> {
    state.prompts.list()
}

#[tauri::command]
fn save_stored_prompt(prompt: work::StoredPrompt, state: State<'_, AppState>) -> Result<(), String> {
    state.prompts.save(prompt)
}

#[tauri::command]
fn delete_stored_prompt(name: String, state: State<'_, AppState>) -> Result<bool, String> {
    state.prompts.delete(&name)
}

#[tauri::command]
fn work_run(state: State<'_, AppState>) -> Result<Option<WorkQueue>, String> {
    state.work_run_store.get()
}

/// Queue one stored prompt against a set of projects.
///
/// Replaces any previous run: two at once would compete for the same fleet
/// slots, and neither report would describe what actually happened.
#[tauri::command]
async fn start_work_run(
    prompt: String,
    projects: Vec<PathBuf>,
    state: State<'_, AppState>,
) -> Result<Option<WorkQueue>, String> {
    let client = daemon_client(&state)?;
    let prompts = state.prompts.clone();
    let work_run_store = state.work_run_store.clone();
    run_blocking("start_work_run", move || {
        start_work_run_with(prompt, projects, client, work_run_store, prompts)
    })
    .await
}

fn start_work_run_with(
    prompt: String,
    projects: Vec<PathBuf>,
    client: DaemonClient,
    work_run_store: work::WorkRunStore,
    prompts: work::PromptLibrary,
) -> Result<Option<WorkQueue>, String> {
    if prompts.get(&prompt)?.is_none() {
        return Err(format!("no stored prompt named {prompt}"));
    }
    let named: Vec<(String, PathBuf)> = projects
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            (name, path)
        })
        .collect();
    let queue = WorkQueue::new(&prompt, &named).map_err(|error| error.to_string())?;
    work_run_store.set(Some(queue))?;
    drive_work_run_with(&client, &work_run_store, &prompts)?;
    work_run_store.get()
}

/// Accept the risk on a project flagged for a dirty tree.
#[tauri::command]
async fn approve_flagged_project(path: PathBuf, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    let prompts = state.prompts.clone();
    let work_run_store = state.work_run_store.clone();
    run_blocking("approve_flagged_project", move || {
        work_run_store
            .update(|queue| queue.approve_flagged(&path))?
            .transpose()
            .map_err(|error| error.to_string())?;
        drive_work_run_with(&client, &work_run_store, &prompts)
    })
    .await
}

#[tauri::command]
async fn skip_work_project(path: PathBuf, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    let prompts = state.prompts.clone();
    let work_run_store = state.work_run_store.clone();
    run_blocking("skip_work_project", move || {
        work_run_store
            .update(|queue| queue.set_state(&path, EntryState::Skipped))?
            .transpose()
            .map_err(|error| error.to_string())?;
        drive_work_run_with(&client, &work_run_store, &prompts)
    })
    .await
}

#[tauri::command]
async fn set_work_run_paused(paused: bool, state: State<'_, AppState>) -> Result<(), String> {
    let client = if paused {
        None
    } else {
        Some(daemon_client(&state)?)
    };
    let prompts = state.prompts.clone();
    let work_run_store = state.work_run_store.clone();
    run_blocking("set_work_run_paused", move || {
        work_run_store.update(|queue| queue.paused = paused)?;
        if let Some(client) = client {
            drive_work_run_with(&client, &work_run_store, &prompts)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
fn clear_work_run(state: State<'_, AppState>) -> Result<(), String> {
    state.work_run_store.set(None)
}

/// Start as many of the run's projects as the fleet has room for.
///
/// Admission is the fleet's decision, not the queue's: this asks for one slot at
/// a time and stops when the answer is no. Deciding here how many agents the
/// machine can run would duplicate a budget that already exists, and drift.
fn drive_work_run_with(
    client: &DaemonClient,
    work_run_store: &work::WorkRunStore,
    prompts: &work::PromptLibrary,
) -> Result<(), String> {
    loop {
        let Some(queue) = work_run_store.get()? else {
            return Ok(());
        };
        if queue.paused || queue.is_finished() {
            return Ok(());
        }

        // Before asking for a slot, give up on work that has waited longer than
        // it is worth. A run with no deadline launches whatever was queued hours
        // ago the moment a slot frees, and by then the tree has usually moved.
        let expired = work_run_store
            .update(|queue| {
                queue.expire_stale(
                    terminalai_core::work_queue::DEFAULT_WAIT_DEADLINE,
                    std::time::SystemTime::now(),
                )
            })?
            .unwrap_or(0);
        if expired > 0 {
            // Loop rather than continue past it: the store has changed under us.
            continue;
        }

        let admission = match daemon_response(client, Request::Snapshot)? {
            Response::Snapshot { admission, .. } => admission,
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected snapshot response: {other:?}")),
        };
        // One decision, the daemon's: the slot cap, the spend ceiling and the
        // memory budget all report through the same field, so this loop cannot
        // enforce a different set of limits than the gate does.
        if admission.admission_block.is_some() {
            return Ok(());
        }
        // Credentials the agent has already said are gone. Holding is the whole
        // point: draining the run turns one expired login into one failure per
        // project, none of which says what actually happened.
        if !admission.expired_auth.is_empty() {
            return Ok(());
        }
        let Some(entry) = queue.next_pending().cloned() else {
            return Ok(());
        };

        // Checked now rather than when the run was created: a tree the operator
        // cleaned up in the meantime should not stay flagged from an hour ago.
        let tree = terminalai_core::work_queue::tree_state(&entry.project);
        if !tree.is_clean() {
            work_run_store
                .update(|queue| queue.set_state(&entry.project, EntryState::Flagged { tree }))?;
            continue;
        }

        let text = match prompts.get(&queue.prompt)? {
            Some(prompt) => prompt.text,
            None => {
                work_run_store.update(|queue| {
                    queue.set_state(
                        &entry.project,
                        EntryState::Failed {
                            detail: "the stored prompt was deleted while the run was going".into(),
                        },
                    )
                })?;
                continue;
            }
        };

        // Launched with no initial prompt: the text goes on the session own
        // prompt queue, which delivers it as a bracketed-paste pty write. As an
        // argument it would reach a command line, and these prompts are
        // kilobytes of prose containing characters Windows quoting mangles.
        let spec = LaunchSpec {
            cwd: entry.project.clone(),
            ..LaunchSpec::default()
        };
        let launched = daemon_response(
            client,
            Request::Launch {
                spec: Box::new(spec),
                configured_path: None,
            },
        )?;
        let id = match launched {
            Response::Launched { id, .. } => id,
            Response::Error { message } => {
                work_run_store.update(|queue| {
                    queue.set_state(&entry.project, EntryState::Failed { detail: message })
                })?;
                continue;
            }
            other => return Err(format!("unexpected launch response: {other:?}")),
        };
        match daemon_response(
            client,
            Request::EnqueuePrompt {
                id: id.clone(),
                text,
            },
        )? {
            Response::Enqueued { .. } => {
                work_run_store.update(|queue| {
                    queue.set_state(
                        &entry.project,
                        EntryState::Running {
                            session: id.clone(),
                        },
                    )
                })?;
            }
            Response::Error { message } => {
                // The session exists but has no instruction, which is worse than
                // not starting it at all: say so rather than leave it running.
                work_run_store.update(|queue| {
                    queue.set_state(
                        &entry.project,
                        EntryState::Failed {
                            detail: format!(
                                "session started but the prompt could not be queued: {message}"
                            ),
                        },
                    )
                })?;
            }
            other => return Err(format!("unexpected queue response: {other:?}")),
        }
    }
}

fn finish_work_run_session(
    client: &DaemonClient,
    work_run_store: &work::WorkRunStore,
    prompts: &work::PromptLibrary,
    session: &SessionId,
) -> Result<(), String> {
    let finished = work_run_store
        .update(|queue| queue.finish_session(session))?
        .unwrap_or(false);
    if finished {
        drive_work_run_with(client, work_run_store, prompts)?;
    }
    Ok(())
}

/// Every repository under the registered roots.
///
/// Discovered fresh on every call rather than cached: the list's value is being
/// current, and a cache would need invalidation nobody would remember to
/// trigger when a repository is cloned.
#[tauri::command]
async fn list_projects(
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::project::Project>, String> {
    let project_roots = state.project_roots.clone();
    run_blocking("list_projects", move || project_roots.projects()).await
}

/// Every project with what its roadmap says.
///
/// Separate from `list_projects` because it costs a file read per project, and
/// the launcher's folder dropdown does not need it.
#[tauri::command]
async fn scan_projects(
    state: State<'_, AppState>,
) -> Result<Vec<projects::ScannedProject>, String> {
    let project_roots = state.project_roots.clone();
    run_blocking("scan_projects", move || project_roots.scanned()).await
}

#[tauri::command]
fn list_project_roots(state: State<'_, AppState>) -> Result<Vec<PathBuf>, String> {
    state.project_roots.list()
}

#[tauri::command]
fn add_project_root(path: PathBuf, state: State<'_, AppState>) -> Result<(), String> {
    state.project_roots.add(path)
}

#[tauri::command]
fn remove_project_root(path: PathBuf, state: State<'_, AppState>) -> Result<bool, String> {
    state.project_roots.remove(&path)
}

/// Offer every built-in preset again.
///
/// Hiding one is otherwise a one-way door: a built-in exists only in code, so
/// there is no way to recreate it by hand — the name would collide with the
/// built-in it was meant to replace.
#[tauri::command]
fn restore_builtin_presets(state: State<'_, AppState>) -> Result<usize, String> {
    state.presets.restore_builtins()
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
async fn preflight_report() -> Result<PreflightReport, String> {
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
    let endpoint = DaemonClient::connect_with_timeout(PREFLIGHT_DAEMON_TIMEOUT)
        .ok()
        .and_then(|client| client.hook_endpoint().ok());
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

fn blocked_hook_preflight(
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

fn cleanup_http_hooks_at(home: &Path, executable: &Path) -> Result<(), String> {
    let path = terminalai_core::hook_config_path(Agent::Claude, home, None);
    terminalai_core::downgrade_claude_http_hooks_at(&path, executable)
        .map(|_| ())
        .map_err(|error| format!("clean up Claude HTTP hooks: {error}"))
}

fn cleanup_http_hooks() -> Result<(), String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    cleanup_http_hooks_at(&home, &executable)
}

fn install_daemon_client(
    app: &tauri::AppHandle,
    state: &State<'_, AppState>,
    client: DaemonClient,
) -> Result<(), String> {
    client
        .subscribe()
        .map_err(|error| format!("subscribe to daemon protocol v{PROTOCOL_VERSION}: {error}"))?;
    bridge_daemon_events(
        app,
        &client,
        state.output_channels.clone(),
        state.work_run_store.clone(),
        state.prompts.clone(),
    );
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
    output_channels: OutputChannels,
    work_run_store: work::WorkRunStore,
    prompts: work::PromptLibrary,
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
    let work_run_client = client.clone();
    // Toast clicks arrive on a WinRT thread; the listener moves that work onto
    // a thread the Tauri runtime knows about.
    let (toast_activations, toast_clicks) = std::sync::mpsc::channel();
    spawn_toast_activation_listener(app.clone(), toast_clicks);
    let _ = thread::Builder::new()
        .name("terminalai-ui-events".into())
        .spawn(move || {
            let mut waiting = initial_waiting;
            let mut rendered_waiting = None;
            // Which sessions already have a toast out. Keyed by id and status so
            // a session that moves from one attention state to another toasts
            // again, while repeated hook deliveries for the same state do not.
            let mut toasted: HashMap<SessionId, SessionStatus> = HashMap::new();
            // A missing Start Menu shortcut makes every toast silently fail.
            // Reported once rather than on every attention event.
            let mut toast_failed = false;
            update_taskbar_waiting_count(&app, waiting.len());
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
                        queue_output(&output_channels, id, data);
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
                                if session.status == SessionStatus::Exited {
                                    if let Err(error) = finish_work_run_session(
                                        &work_run_client,
                                        &work_run_store,
                                        &prompts,
                                        &session.id,
                                    ) {
                                        eprintln!(
                                            "TerminalAI: could not advance the work run after {} exited: {error}",
                                            session.id
                                        );
                                    }
                                }
                                if is_waiting_session(session) {
                                    waiting.insert(session.id.clone());
                                } else {
                                    waiting.remove(&session.id);
                                }
                                maybe_toast(
                                    session,
                                    &mut toasted,
                                    &toast_activations,
                                    &mut toast_failed,
                                );
                            }
                            RegistryEvent::SessionRemoved { id } => {
                                if let Err(error) = finish_work_run_session(
                                    &work_run_client,
                                    &work_run_store,
                                    &prompts,
                                    id,
                                ) {
                                    eprintln!(
                                        "TerminalAI: could not advance the work run after {id} was removed: {error}"
                                    );
                                }
                                waiting.remove(id);
                                toasted.remove(id);
                            }
                            _ => {}
                        }
                        if rendered_waiting != Some(waiting.len()) {
                            update_taskbar_waiting_count(&app, waiting.len());
                            rendered_waiting = Some(waiting.len());
                        }
                        flush_output_batches(&output_channels);
                        next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                        if !matches!(&event, RegistryEvent::AgentEvent { .. })
                            && app.emit("terminalai:event", event).is_err()
                        {
                            break;
                        }
                    }
                    Some(Err(RecvTimeoutError::Timeout)) => {
                        flush_output_batches(&output_channels);
                        next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                    }
                    Some(Err(RecvTimeoutError::Disconnected)) | None => {
                        flush_output_batches(&output_channels);
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

/// Raise a toast when a session newly wants the operator.
///
/// Keyed on (id, status): a session moving from `AwaitingInput` to
/// `NeedsApproval` is a new thing to say, but the same status arriving twice —
/// which it does, because hooks fire per tool call — is not.
fn maybe_toast(
    session: &Session,
    toasted: &mut HashMap<SessionId, SessionStatus>,
    activations: &std::sync::mpsc::Sender<toast::ToastActivation>,
    failed: &mut bool,
) {
    if !toast::wants_attention(session.status) {
        // Leaving an attention state clears the memo, so the next one toasts.
        toasted.remove(&session.id);
        return;
    }
    if toasted.get(&session.id) == Some(&session.status) {
        return;
    }
    toasted.insert(session.id.clone(), session.status);
    if let Err(error) =
        toast::raise_attention_toast(APP_USER_MODEL_ID, session, activations.clone())
    {
        if !*failed {
            // Once. A fleet of thirty would otherwise print this per event, and
            // the cause is always the same missing Start Menu shortcut.
            *failed = true;
            eprintln!(
                "terminalai: desktop notifications unavailable ({error}); run the Start-Menu shortcut preflight fix"
            );
        }
    }
}

/// Focus the session a clicked toast names, and raise the window.
fn spawn_toast_activation_listener(
    app: tauri::AppHandle,
    activations: std::sync::mpsc::Receiver<toast::ToastActivation>,
) {
    // `thread::Builder::spawn`, like every other spawn in the workspace: the
    // bare form panics where this returns an error, and a toast listener is not
    // worth taking the window process down for.
    let listener = thread::Builder::new()
        .name("terminalai-toast-activation".into())
        .spawn(move || {
        // The WinRT handler only sends on this channel; everything that touches
        // Tauri happens here, on a thread the runtime knows about.
        while let Ok(toast::ToastActivation::Focus(id)) = activations.recv() {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
            let _ = app.emit("terminalai:focus-session", id);
        }
    });
    if let Err(error) = listener {
        // Toasts still fire; clicking one just will not focus its session.
        eprintln!(
            "terminalai: toast activations will not focus their session ({error}); \
             could not start the listener thread"
        );
    }
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

fn queue_output(output_channels: &OutputChannels, id: SessionId, data: Vec<u8>) {
    let route = output_channels
        .lock()
        .ok()
        .and_then(|channels| channels.get(&id).cloned());
    let Some(route) = route else {
        return;
    };
    if route.queue(data).is_err() {
        remove_output_route(&id, &route, output_channels);
    }
}

fn flush_output_batches(output_channels: &OutputChannels) {
    let routes = output_channels
        .lock()
        .ok()
        .map(|channels| {
            channels
                .iter()
                .map(|(id, route)| (id.clone(), route.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (id, route) in routes {
        if route.flush().is_err() {
            remove_output_route(&id, &route, output_channels);
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
    let app_result = builder
        .invoke_handler(tauri::generate_handler![
            app_version,
            fleet_snapshot,
            review_snapshot,
            external_sessions,
            mark_reviewed,
            admission_config,
            set_admission,
            land_session,
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
            grid_snapshot,
            subscribe_output,
            stream_scrollback,
            stream_scrollback_history,
            broadcast_prompt,
            queued_prompts,
            enqueue_prompt,
            edit_queued_prompt,
            remove_queued_prompt,
            reorder_queued_prompt,
            pause_queue,
            resume_queue,
            attach_session_output,
            revive_session,
            archive_session,
            list_presets,
            list_templates,
            save_preset,
            delete_preset,
            restore_builtin_presets,
            list_projects,
            scan_projects,
            list_stored_prompts,
            save_stored_prompt,
            delete_stored_prompt,
            work_run,
            start_work_run,
            approve_flagged_project,
            skip_work_project,
            set_work_run_paused,
            clear_work_run,
            list_project_roots,
            add_project_root,
            remove_project_root,
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
            let project_roots = projects::ProjectRoots::load_default().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            let prompts = work::PromptLibrary::load_default().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            let work_run_store = work::WorkRunStore::load_default().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            let output_channels = Arc::new(Mutex::new(HashMap::new()));
            if let Some(client) = client.as_ref() {
                bridge_daemon_events(
                    app.handle(),
                    client,
                    output_channels.clone(),
                    work_run_store.clone(),
                    prompts.clone(),
                );
            }
            app.manage(AppState {
                client: Mutex::new(client),
                presets,
                project_roots,
                prompts,
                work_run_store,
                output_channels,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|error| error.to_string());
    let cleanup_result = cleanup_http_hooks();
    match (app_result, cleanup_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error),
    }
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
    let event = match parse_hook_in(agent, &input, std::env::current_dir().ok()) {
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
    let hook_token = std::env::var("TERMINALAI_HOOK_TOKEN").ok();
    match client.call_with_timeout(
        Request::Hook { event, hook_token },
        timeout,
    ) {
        Ok(Response::Hook { .. }) | Ok(Response::Ok) => {}
        Ok(other) => eprintln!("ignoring unexpected hook response: {other:?}"),
        Err(error) => eprintln!("ignoring hook delivery failure: {error}"),
    }
    0
}

fn main() {
    let _logging = terminalai_daemon::init_logging_with_prefix("terminalai-app");
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
        assert_eq!(PROTOCOL_VERSION, 3);
    }

    #[test]
    fn attaching_mid_stream_replays_each_pty_byte_once_in_order() {
        let replay = b"prompt\r\noutput> ".to_vec();
        let pending = b"output> next\r\n".to_vec();
        let overlap = replay_overlap(&replay, &pending);
        let mut rendered = replay;
        rendered.extend_from_slice(&pending[overlap..]);

        assert_eq!(rendered, b"prompt\r\noutput> next\r\n");
        assert_eq!(replay_overlap(b"abc", b"xyz"), 0);
    }

    #[test]
    fn hook_invocation_bypasses_the_gui_shell() {
        assert!(is_hook_invocation(&["hook".into(), "claude".into()]));
        assert!(!is_hook_invocation(&["--help".into()]));
        assert!(!is_hook_invocation(&[]));
    }

    #[test]
    fn managed_hook_policy_is_a_distinct_non_fixable_preflight_state() {
        let policy = terminalai_core::ManagedHookPolicy {
            sources: vec![r"C:\Program Files\ClaudeCode\managed-settings.json".into()],
            disable_all_hooks: true,
            allow_managed_hooks_only: false,
            strict_plugin_hooks: false,
        };
        let check = blocked_hook_preflight(&policy, true, "Claude: installed");
        assert_eq!(check.state, "blocked");
        assert!(!check.can_fix);
        assert_eq!(
            check.detected,
            r"hooks installed but disabled by policy at C:\Program Files\ClaudeCode\managed-settings.json"
        );
        assert!(check
            .detail
            .expect("policy detail")
            .contains("disableAllHooks=true"));
    }

    #[test]
    fn an_http_hook_handler_is_removed_when_its_endpoint_dies() {
        let home = std::env::temp_dir().join(format!(
            "terminalai-app-hooks-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let path = home.join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().expect("settings parent")).expect("home");
        let transport = HookTransport::Http {
            url: "http://127.0.0.1:43123/hooks/claude".into(),
            host: "127.0.0.1:43123".into(),
            bearer_token: "shutdown-token".into(),
        };
        terminalai_core::install_hooks_at_with_transport(Agent::Claude, &path, &transport)
            .expect("install HTTP hook");

        cleanup_http_hooks_at(&home, Path::new("terminalai.exe")).expect("shutdown cleanup");

        let cleaned = std::fs::read_to_string(&path).expect("read cleaned settings");
        assert!(!cleaned.contains("127.0.0.1"));
        assert!(!cleaned.contains("Bearer shutdown-token"));
        assert!(!cleaned.contains("\"type\": \"http\""));
        assert!(cleaned.contains(terminalai_core::MANAGED_MARKER));
        let _ = std::fs::remove_dir_all(home);
    }
}
