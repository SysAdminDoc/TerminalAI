//! Tauri commands for the operator's workspace and saved configuration.
//!
//! These commands are intentionally read/write boundaries around the local
//! stores and the daemon's workspace inventory. Keeping them together leaves
//! the application entry point responsible for composition rather than for
//! every preset, project, layout, and prompt detail.

use std::path::PathBuf;

use tauri::State;
use terminalai_daemon::{Request, Response, MAX_HISTORY_BYTES};

use super::daemon::{client as daemon_client, response as daemon_response, run_blocking};
use super::preset::Preset;
use super::projects;
use super::state::AppState;
use super::work;
use super::workingset;

#[tauri::command]
pub(crate) fn list_working_sets(
    state: State<'_, AppState>,
) -> Result<Vec<workingset::WorkingSet>, String> {
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
pub(crate) async fn save_working_set(
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
pub(crate) fn delete_working_set(name: String, state: State<'_, AppState>) -> Result<bool, String> {
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
pub(crate) async fn restore_working_set(
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
                            Ok(Response::Error { message }) => outcome.pin_refused = Some(message),
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
pub(crate) async fn search_fleet(
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
            max_bytes: MAX_HISTORY_BYTES,
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
pub(crate) async fn stale_worktrees(
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
pub(crate) async fn reap_worktree(
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

#[tauri::command]
pub(crate) fn list_presets(state: State<'_, AppState>) -> Result<Vec<Preset>, String> {
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
pub(crate) fn list_templates(
    cwd: PathBuf,
) -> Result<Vec<terminalai_core::template::Template>, String> {
    terminalai_core::template::load(&cwd).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn save_preset(preset: Preset, state: State<'_, AppState>) -> Result<(), String> {
    state.presets.save(preset)
}

/// The stored prompt library.
#[tauri::command]
pub(crate) fn list_stored_prompts(
    state: State<'_, AppState>,
) -> Result<Vec<work::StoredPrompt>, String> {
    state.prompts.list()
}

#[tauri::command]
pub(crate) fn save_stored_prompt(
    prompt: work::StoredPrompt,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.prompts.save(prompt)
}

#[tauri::command]
pub(crate) fn delete_stored_prompt(
    name: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    state.prompts.delete(&name)
}

/// Every repository under the registered roots.
///
/// Discovered fresh on every call rather than cached: the list's value is being
/// current, and a cache would need invalidation nobody would remember to
/// trigger when a repository is cloned.
#[tauri::command]
pub(crate) async fn list_projects(
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
pub(crate) async fn scan_projects(
    state: State<'_, AppState>,
) -> Result<Vec<projects::ScannedProject>, String> {
    let project_roots = state.project_roots.clone();
    run_blocking("scan_projects", move || project_roots.scanned()).await
}

#[tauri::command]
pub(crate) fn list_project_roots(state: State<'_, AppState>) -> Result<Vec<PathBuf>, String> {
    state.project_roots.list()
}

#[tauri::command]
pub(crate) fn add_project_root(path: PathBuf, state: State<'_, AppState>) -> Result<(), String> {
    state.project_roots.add(path)
}

#[tauri::command]
pub(crate) fn remove_project_root(
    path: PathBuf,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    state.project_roots.remove(&path)
}

/// Offer every built-in preset again.
///
/// Hiding one is otherwise a one-way door: a built-in exists only in code, so
/// there is no way to recreate it by hand — the name would collide with the
/// built-in it was meant to replace.
#[tauri::command]
pub(crate) fn restore_builtin_presets(state: State<'_, AppState>) -> Result<usize, String> {
    state.presets.restore_builtins()
}

#[tauri::command]
pub(crate) fn delete_preset(name: String, state: State<'_, AppState>) -> Result<bool, String> {
    state.presets.delete(&name)
}

#[tauri::command]
pub(crate) fn pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Choose project folder")
        .pick_folder()
        .map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
pub(crate) fn pick_extra_dirs() -> Vec<String> {
    rfd::FileDialog::new()
        .set_title("Choose extra writable folders")
        .pick_folders()
        .unwrap_or_default()
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}
