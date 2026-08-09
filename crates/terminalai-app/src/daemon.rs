//! Shared daemon-call policy for Tauri commands.
//!
//! Commands should describe the request and its response shape. The mechanics
//! of obtaining the current client, moving blocking IPC off the async runtime,
//! and rejecting an unexpected response belong here so a new command cannot
//! accidentally invent a second error path.

use tauri::State;
use terminalai_daemon::{DaemonClient, Request, Response};

use super::state::AppState;

pub(crate) async fn run_blocking<T, F>(label: &'static str, task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| format!("{label} background task failed: {error}"))?
}

pub(crate) fn response(client: &DaemonClient, request: Request) -> Result<Response, String> {
    client.call(request).map_err(|error| error.to_string())
}

pub(crate) fn client(state: &State<'_, AppState>) -> Result<DaemonClient, String> {
    state
        .client
        .lock()
        .map_err(|_| "daemon client state is poisoned".to_string())?
        .clone()
        .ok_or_else(|| "daemon is unavailable; run preflight checks and retry".to_string())
}

pub(crate) fn require_ok(response: Response) -> Result<(), String> {
    match response {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
}

pub(crate) fn expect_ok(state: &State<'_, AppState>, request: Request) -> Result<(), String> {
    match response(&client(state)?, request)? {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}
