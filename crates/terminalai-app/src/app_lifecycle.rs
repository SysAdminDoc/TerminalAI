//! Application startup, daemon attachment, and shutdown cleanup.
//!
//! The Tauri entry point still owns the generated command registry because the
//! build script audits that source location. Everything around that registry —
//! daemon discovery, persisted stores, event wiring, and hook cleanup — lives
//! here so startup policy has one boundary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::Manager;
use terminalai_daemon::{DaemonClient, IpcError, PROTOCOL_VERSION};

use super::events::bridge_daemon_events;
use super::preset::PresetStore;
use super::projects;
use super::state::AppState;
use super::work;
use super::workingset;

pub(crate) fn connect_or_start_daemon() -> Result<DaemonClient, String> {
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

pub(crate) fn cleanup_http_hooks_at(home: &Path, executable: &Path) -> Result<(), String> {
    let path = terminalai_core::hook_config_path(terminalai_core::agent::Agent::Claude, home, None);
    terminalai_core::downgrade_claude_http_hooks_at(&path, executable)
        .map(|_| ())
        .map_err(|error| format!("clean up Claude HTTP hooks: {error}"))
}

pub(crate) fn cleanup_http_hooks() -> Result<(), String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    cleanup_http_hooks_at(&home, &executable)
}

pub(crate) fn connect_for_app() -> Option<DaemonClient> {
    if cfg!(feature = "wdio") {
        return None;
    }
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
}

pub(crate) fn install_daemon_client(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
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

pub(crate) fn setup(
    app: &mut tauri::App,
    client: Option<DaemonClient>,
) -> Result<(), Box<dyn std::error::Error>> {
    let presets = PresetStore::load_default()
        .map_err(|error| Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>)?;
    let project_roots = projects::ProjectRoots::load_default()
        .map_err(|error| Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>)?;
    let prompts = work::PromptLibrary::load_default()
        .map_err(|error| Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>)?;
    let work_run_store = work::WorkRunStore::load_default()
        .map_err(|error| Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>)?;
    let work_schedule_store = work::WorkScheduleStore::load_default()
        .map_err(|error| Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>)?;
    let working_sets = workingset::WorkingSetStore::load_default()
        .map_err(|error| Box::new(std::io::Error::other(error)) as Box<dyn std::error::Error>)?;
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
}
