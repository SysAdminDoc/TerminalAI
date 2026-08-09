//! Tauri commands for the shell's overview and policy surfaces.
//!
//! These are read-oriented dashboard/history calls plus the daemon-wide
//! admission settings. They share a protocol boundary but do not belong to
//! the live-session or workspace command families.

use tauri::State;
use terminalai_daemon::{Request, Response};

use super::daemon::{client as daemon_client, response as daemon_response, run_blocking};
use super::state::{AppState, FleetSnapshot, ReviewSnapshot};

#[tauri::command]
pub(crate) fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[tauri::command]
pub(crate) fn fleet_snapshot(state: State<'_, AppState>) -> Result<FleetSnapshot, String> {
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
pub(crate) fn review_snapshot(state: State<'_, AppState>) -> Result<ReviewSnapshot, String> {
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
pub(crate) async fn external_sessions(
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
pub(crate) async fn session_history(
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

/// Read the daemon-wide admission policy for the settings dialog.
#[tauri::command]
pub(crate) fn admission_config(
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
pub(crate) fn set_admission(
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
