#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dpi;
mod events;
mod output;
mod preset;
mod projects;
mod preflight;
mod restart;
mod toast;
mod work;
mod workflows;
mod workingset;

use terminalai_core::schedule::{FiringResult, ScheduleFiring, WorkSchedule};
use terminalai_core::work_queue::{EntryState, WorkQueue};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use std::{io, io::Read};

use preset::{Preset, PresetStore};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Emitter, Manager, State};
use terminalai_core::agent::Agent;
use terminalai_core::launch::LaunchSpec;
use terminalai_core::{
    fleet_progress, parse_hook_in, AdmissionSnapshot, AgentCapabilities, FleetProgress,
    HookTransport, LogEntry, ProgressStatus, RegistryEvent, ReviewItem, Session, SessionId,
    SessionStatus, TaskProgress, MAX_LOG_ENTRIES,
};
use terminalai_daemon::{
    DaemonClient, HookEndpoint, IpcError, Request, Response, PROTOCOL_VERSION,
};
use events::bridge_daemon_events;
use preflight::{open_external_url, preflight_fix, preflight_report};
use output::{
    register_output_channel, remove_output_route, replay_overlap, send_raw, OutputChannels,
};
use workflows::{
    approve_flagged_project, clear_work_run, clear_work_schedule, finish_work_run_session,
    fire_due_schedule, set_work_run_paused, set_work_schedule, set_work_schedule_paused,
    skip_work_project, start_work_run, work_run, work_schedule,
};

struct AppState {
    client: Mutex<Option<DaemonClient>>,
    presets: PresetStore,
    project_roots: projects::ProjectRoots,
    prompts: work::PromptLibrary,
    work_run_store: work::WorkRunStore,
    work_schedule_store: work::WorkScheduleStore,
    working_sets: workingset::WorkingSetStore,
    output_channels: OutputChannels,
}

#[derive(Debug, Serialize)]
struct FleetSnapshot {
    sessions: Vec<Session>,
    focused: Option<SessionId>,
    admission: AdmissionSnapshot,
    store_quarantine: Option<String>,
    store_write_error: Option<String>,
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
            store_write_error,
        } => Ok(FleetSnapshot {
            sessions,
            focused,
            admission,
            store_quarantine,
            store_write_error,
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

/// Sessions this supervisor finished, newest first.
///
/// The archive has always been written and never read back for anything but the
/// id counter. It carries no PTY handle and no output — only what is needed to
/// see what ran and to start the same thing again.
#[tauri::command]
async fn session_history(
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::ArchivedSession>, String> {
    let client = daemon_client(&state)?;
    run_blocking("session_history", move || {
        match daemon_response(&client, Request::SessionHistory)? {
            Response::SessionHistory { archives } => Ok(archives),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected session-history response: {other:?}")),
        }
    })
    .await
}

#[tauri::command]
fn list_working_sets(state: State<'_, AppState>) -> Result<Vec<workingset::WorkingSet>, String> {
    Ok(state.working_sets.list())
}

/// Capture the live fleet as a named layout.
///
/// The specs come from the daemon, not from the caller. A `Session` does not
/// carry the spec that produced it — it is sent on every status change and the
/// spec is large and unchanging — so the window has never had them, and a
/// caller-supplied spec would let a layout be saved that does not describe
/// anything actually running.
#[tauri::command]
async fn save_working_set(
    name: String,
    group_by: Option<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let client = daemon_client(&state)?;
    let store = state.working_sets.clone();
    run_blocking("save_working_set", move || {
        let specs = match daemon_response(&client, Request::FleetSpecs)? {
            Response::FleetSpecs { specs } => specs,
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected fleet-spec response: {other:?}")),
        };
        let members: Vec<workingset::WorkingSetMember> = specs
            .into_iter()
            .map(|entry| workingset::WorkingSetMember {
                configured_path: Some(entry.spec.cwd.clone()),
                spec: *entry.spec,
                pinned: entry.pinned,
            })
            .collect();
        let count = members.len();
        store.save(workingset::WorkingSet {
            name,
            members,
            group_by,
        })?;
        Ok(count)
    })
    .await
}

#[tauri::command]
fn delete_working_set(name: String, state: State<'_, AppState>) -> Result<bool, String> {
    state.working_sets.delete(&name)
}

/// Relaunch a saved layout, one member at a time.
///
/// Every member goes through the same `Request::Launch` the launcher uses, so
/// admission, the memory budget, the spend ceiling and the dirty-tree refusal
/// all apply without this function knowing they exist — and the *next* limit
/// added applies too, which a bespoke restore path would silently bypass.
///
/// A refusal is an expected outcome rather than an error: eleven of twelve
/// sessions started is a useful result, and rolling the other eleven back
/// because the twelfth was refused would throw away the work that succeeded.
/// The caller is told, per member, what happened.
#[tauri::command]
async fn restore_working_set(
    name: String,
    state: State<'_, AppState>,
) -> Result<Vec<workingset::RestoreOutcome>, String> {
    let client = daemon_client(&state)?;
    let Some(set) = state.working_sets.get(&name) else {
        return Err(format!("no working set named {name}"));
    };
    run_blocking("restore_working_set", move || {
        let mut outcomes = Vec::with_capacity(set.members.len());
        for member in set.members {
            let label = member
                .spec
                .name
                .clone()
                .unwrap_or_else(|| member.spec.cwd.display().to_string());
            let mut outcome = workingset::RestoreOutcome {
                name: label,
                cwd: member.spec.cwd.clone(),
                id: None,
                queued: false,
                refused: None,
                pin_refused: None,
            };
            let launch = daemon_response(
                &client,
                Request::Launch {
                    spec: Box::new(member.spec),
                    configured_path: member.configured_path,
                },
            );
            match launch {
                Ok(Response::Launched { id, queued }) => {
                    outcome.queued = queued;
                    // The pin is applied only once the row exists. A queued row
                    // exists, so pinning it is legitimate — it will hold a grid
                    // as soon as it starts.
                    if member.pinned {
                        match daemon_response(&client, Request::TogglePin { id: id.clone() }) {
                            Ok(Response::Error { message }) => {
                                outcome.pin_refused = Some(message)
                            }
                            Err(error) => outcome.pin_refused = Some(error.to_string()),
                            Ok(_) => {}
                        }
                    }
                    outcome.id = Some(id.0);
                }
                Ok(Response::Error { message }) => outcome.refused = Some(message),
                Ok(other) => outcome.refused = Some(format!("unexpected response: {other:?}")),
                Err(error) => outcome.refused = Some(error.to_string()),
            }
            outcomes.push(outcome);
        }
        Ok(outcomes)
    })
    .await
}

/// Find a string in every session's retained output.
///
/// A read: it needs no write token and wakes nothing. The bytes searched per
/// session are the daemon's history ceiling, the same budget the focused pane's
/// "load older output" spends — a search that read less than that would answer
/// "not found" about output the operator can see by scrolling.
#[tauri::command]
async fn search_fleet(
    state: State<'_, AppState>,
    needle: String,
    case_sensitive: bool,
) -> Result<Vec<terminalai_core::search::SessionMatches>, String> {
    let client = daemon_client(&state)?;
    run_blocking("search_fleet", move || {
        let request = Request::SearchScrollback {
            query: terminalai_core::search::SearchQuery {
                needle,
                case_sensitive,
            },
            max_bytes: terminalai_daemon::MAX_HISTORY_BYTES,
        };
        match daemon_response(&client, request)? {
            Response::SearchResults { matches, .. } => Ok(matches),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected search response: {other:?}")),
        }
    })
    .await
}

/// Checkouts this tool created that no live session owns.
///
/// Reporting only. What to do about a branch holding unmerged work is the
/// operator's call, and the refusal for that case lives in the core so a caller
/// that skipped this view cannot delete commits either.
#[tauri::command]
async fn stale_worktrees(
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::worktree::StaleWorktree>, String> {
    let client = daemon_client(&state)?;
    run_blocking("stale_worktrees", move || {
        match daemon_response(&client, Request::StaleWorktrees)? {
            Response::StaleWorktrees { worktrees } => Ok(worktrees),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected stale-worktree response: {other:?}")),
        }
    })
    .await
}

/// Remove one surveyed checkout, refusing anything that still holds work.
#[tauri::command]
async fn reap_worktree(
    state: State<'_, AppState>,
    stale: terminalai_core::worktree::StaleWorktree,
) -> Result<(), String> {
    let client = daemon_client(&state)?;
    run_blocking("reap_worktree", move || {
        match daemon_response(
            &client,
            Request::ReapWorktree {
                stale: Box::new(stale),
            },
        )? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected reap response: {other:?}")),
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
/// The landing and what became of the session, in one answer.
#[derive(serde::Serialize)]
struct LandResult {
    #[serde(flatten)]
    outcome: terminalai_core::land::LandOutcome,
    /// Present only when the request asked to archive.
    archive: Option<terminalai_daemon::ArchiveAfterLanding>,
}

#[tauri::command]
async fn land_session(
    request: terminalai_core::land::LandRequest,
    state: State<'_, AppState>,
) -> Result<LandResult, String> {
    let client = daemon_client(&state)?;
    run_blocking("land_session", move || {
        match daemon_response(
            &client,
            Request::Land {
                request: Box::new(request),
            },
        )? {
            Response::Land { outcome, archive } => Ok(LandResult { outcome, archive }),
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
        state.work_schedule_store.clone(),
        state.prompts.clone(),
    );
    *state
        .client
        .lock()
        .map_err(|_| "daemon client state is poisoned".to_string())? = Some(client);
    Ok(())
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
            session_history,
            search_fleet,
            list_working_sets,
            save_working_set,
            delete_working_set,
            restore_working_set,
            stale_worktrees,
            reap_worktree,
            mark_reviewed,
            admission_config,
            set_admission,
            land_session,
            preview_launch,
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
            work_schedule,
            set_work_schedule,
            set_work_schedule_paused,
            clear_work_schedule,
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
            let work_schedule_store = work::WorkScheduleStore::load_default().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            let working_sets = workingset::WorkingSetStore::load_default().map_err(|error| {
                Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>
            })?;
            let output_channels = Arc::new(Mutex::new(HashMap::new()));
            if let Some(client) = client.as_ref() {
                bridge_daemon_events(
                    app.handle(),
                    client,
                    output_channels.clone(),
                    work_run_store.clone(),
                    work_schedule_store.clone(),
                    prompts.clone(),
                );
            }
            app.manage(AppState {
                client: Mutex::new(client),
                presets,
                project_roots,
                prompts,
                work_run_store,
                work_schedule_store,
                working_sets,
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
    // Before anything creates a window, which is the documented requirement and
    // the reason this is the first thing after logging. Awareness is a process
    // property decided by whoever declares it first; leaving it inherited meant
    // every monitor and window measurement was virtualized on a 125% display
    // while still looking plausible.
    dpi::declare_and_report();
    terminalai_daemon::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if is_hook_invocation(&args) {
        std::process::exit(run_hook_cli(&args[1..]));
    }
    // After the hook branch, so a hook invocation never registers: it is a
    // short-lived adapter, and restarting one would relaunch it with no stdin.
    if restart::was_restarted(&args) {
        tracing::info!("relaunched by Windows after a crash, hang or update");
    }
    restart::register();
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
            let error = preflight::validate_external_url(refused)
                .expect_err(&format!("{refused:?} must not be opened"));
            assert!(error.starts_with("refused"), "{refused:?} gave {error}");
        }
    }

    #[test]
    fn a_link_carrying_control_characters_is_refused() {
        let error = preflight::validate_external_url("https://example.com/\u{1b}]0;pwned\u{7}")
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
            let target = preflight::validate_external_url(allowed)
                .unwrap_or_else(|error| panic!("{allowed:?} should open: {error}"));
            assert_eq!(target, allowed.trim());
        }
    }

    #[test]
    fn protocol_version_is_pinned_for_the_shell() {
        assert_eq!(PROTOCOL_VERSION, 4);
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
        let check = preflight::blocked_hook_preflight(&policy, true, "Claude: installed");
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
    fn hook_preflight_does_not_equate_configuration_with_delivery() {
        assert_eq!(
            preflight::preflight_hook_state(true, false, false, false),
            "installed, not yet proven"
        );
        assert_eq!(
            preflight::preflight_hook_state(true, false, false, true),
            "installed and firing"
        );
        assert_eq!(preflight::preflight_hook_state(false, false, false, true), "missing");
        assert_eq!(preflight::preflight_hook_state(true, true, false, true), "disabled");
        assert_eq!(preflight::preflight_hook_state(true, false, true, true), "stale");
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

    #[test]
    fn a_schedule_does_not_fire_over_a_run_that_is_still_going() {
        // Starting a run replaces the previous one. Firing on top of forty
        // working projects would destroy the report the operator was going to
        // read and put a second agent on the first one's uncommitted edits.
        assert_eq!(
            workflows::previous_run_blocking(None),
            None,
            "a schedule with no run behind it must fire"
        );

        let mut queue = WorkQueue::new(
            "Drain the roadmap",
            &[("shop".into(), PathBuf::from("/repos/shop"))],
        )
        .expect("queue");
        assert!(
            workflows::previous_run_blocking(Some(&queue)).is_some(),
            "a pending run was overwritten by a firing"
        );
        queue
            .set_state(Path::new("/repos/shop"), EntryState::Skipped)
            .expect("state");
        assert!(queue.is_finished());
        assert_eq!(
            workflows::previous_run_blocking(Some(&queue)),
            None,
            "a finished run is a report, and replacing it is what the next firing is for"
        );
    }

    #[test]
    fn only_sessions_that_reported_progress_reach_the_taskbar() {
        // The taskbar is fed from the rows themselves, so a fleet where no
        // agent emits the sequence has to produce no bar rather than a bar at
        // zero -- which would read as "started and got nowhere".
        let spec = terminalai_core::launch::spec_for(Agent::Claude, Path::new("."));
        let quiet = Session::new(SessionId::new(1), &spec);
        let mut reporting = Session::new(SessionId::new(2), &spec);
        reporting.task_progress = Some(TaskProgress::Value { percent: 55 });

        let reported = events::reporting_progress(&[quiet.clone(), reporting]);
        assert_eq!(reported.len(), 1, "a silent session claimed progress");
        assert_eq!(
            fleet_progress(reported.values().copied()),
            Some(FleetProgress {
                status: ProgressStatus::Normal,
                percent: Some(55),
            })
        );
        assert_eq!(
            fleet_progress(events::reporting_progress(&[quiet]).values().copied()),
            None,
            "a fleet that reported nothing produced a bar"
        );
    }
}
