//! Tauri commands that operate on a live session or its terminal surfaces.
//!
//! The browser has one focused renderer, optional Rust-side pinned grids, and a
//! bounded output channel. Keeping those commands together makes the replay and
//! write paths reviewable as one protocol boundary instead of scattering them
//! through the application bootstrap.

use std::path::PathBuf;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;
use terminalai_core::agent::Agent;
use terminalai_core::launch::LaunchSpec;
use terminalai_core::{AgentCapabilities, SessionId};
use terminalai_daemon::{Request, Response};

use super::daemon::{
    client as daemon_client, expect_ok, require_ok, response as daemon_response, run_blocking,
};
use super::output::{register_output_channel, remove_output_route, send_raw};
use super::state::{AppState, LandResult, LaunchReceipt};

#[tauri::command]
pub(crate) fn mark_reviewed(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::MarkReviewed { id })?)
}

/// Land a session's uncommitted work into a target repository, or report the
/// specific reason it was refused.
///
/// The daemon serialises these, so this command blocks while another landing is
/// in flight — that wait is the feature, not an oversight.
/// The landing and what became of the session, in one answer.
#[tauri::command]
pub(crate) async fn land_session(
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
pub(crate) fn grid_snapshot(
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
pub(crate) fn preview_launch(
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
pub(crate) fn agent_capabilities(
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
pub(crate) fn launch_session(
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
pub(crate) fn write_session(
    id: SessionId,
    data: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let client = daemon_client(&state)?;
    // Keep the raw terminal stream intact: the daemon uses its line-ending
    // boundary to distinguish composition from an explicit send.
    require_ok(daemon_response(&client, Request::Write { id, data })?)
}

#[tauri::command]
pub(crate) fn resize_session(
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
pub(crate) fn kill_session(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::Kill { id })?)
}

#[tauri::command]
pub(crate) fn focus_session(
    id: Option<SessionId>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::Focus { id })?)
}

#[tauri::command]
pub(crate) fn mark_read(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    let client = daemon_client(&state)?;
    require_ok(daemon_response(&client, Request::MarkRead { id })?)
}

#[tauri::command]
pub(crate) fn toggle_pin(id: SessionId, state: State<'_, AppState>) -> Result<bool, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::TogglePin { id })? {
        Response::PinChanged { pinned } => Ok(pinned),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected pin response: {other:?}")),
    }
}

#[tauri::command]
pub(crate) fn subscribe_output(
    id: SessionId,
    channel: Channel<InvokeResponseBody>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    register_output_channel(id, channel, &state.output_channels, false).map(|_| ())
}

#[tauri::command]
pub(crate) fn stream_scrollback(
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
pub(crate) fn queued_prompts(
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
pub(crate) fn enqueue_prompt(
    id: SessionId,
    text: String,
    state: State<'_, AppState>,
) -> Result<u64, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::EnqueuePrompt { id, text })? {
        Response::Enqueued { prompt } => Ok(prompt),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected queue response: {other:?}")),
    }
}

#[tauri::command]
pub(crate) fn edit_queued_prompt(
    id: SessionId,
    prompt: u64,
    text: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    expect_ok(&state, Request::EditQueuedPrompt { id, prompt, text })
}

#[tauri::command]
pub(crate) fn remove_queued_prompt(
    id: SessionId,
    prompt: u64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    expect_ok(&state, Request::RemoveQueuedPrompt { id, prompt })
}

#[tauri::command]
pub(crate) fn reorder_queued_prompt(
    id: SessionId,
    prompt: u64,
    to: usize,
    state: State<'_, AppState>,
) -> Result<(), String> {
    expect_ok(&state, Request::ReorderQueuedPrompt { id, prompt, to })
}

#[tauri::command]
pub(crate) fn pause_queue(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    expect_ok(&state, Request::PauseQueue { id })
}

#[tauri::command]
pub(crate) fn resume_queue(id: SessionId, state: State<'_, AppState>) -> Result<(), String> {
    expect_ok(&state, Request::ResumeQueue { id })
}

/// Send one prompt to several sessions, returning what happened to each.
///
/// The per-session result is returned to the caller rather than collapsed into
/// a status, so the UI can say "sent to 5 of 9" instead of "sent" — a broadcast
/// that reports only success is one the operator has to verify by hand.
#[tauri::command]
pub(crate) fn broadcast_prompt(
    ids: Vec<SessionId>,
    text: String,
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::BroadcastResult>, String> {
    let client = daemon_client(&state)?;
    // The same bracketed-paste framing a single reply uses. Without it a
    // multi-line prompt is submitted a line at a time, so the agent acts on
    // the first fragment.
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
pub(crate) fn stream_scrollback_history(
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
pub(crate) fn attach_session_output(
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
pub(crate) fn revive_session(
    id: SessionId,
    state: State<'_, AppState>,
) -> Result<SessionId, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Revive { id })? {
        Response::Revived { id } => Ok(id),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected revive response: {other:?}")),
    }
}

#[tauri::command]
pub(crate) fn archive_session(
    id: SessionId,
    state: State<'_, AppState>,
) -> Result<SessionId, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Archive { id })? {
        Response::Archived { id } => Ok(id),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected archive response: {other:?}")),
    }
}
