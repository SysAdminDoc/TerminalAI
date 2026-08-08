//! Work-run and schedule orchestration commands.

use super::*;

#[tauri::command]
pub(crate) fn work_run(state: State<'_, AppState>) -> Result<Option<WorkQueue>, String> {
    state.work_run_store.get()
}

/// Queue one stored prompt against a set of projects.
///
/// Replaces any previous run: two at once would compete for the same fleet
/// slots, and neither report would describe what actually happened.
#[tauri::command]
pub(crate) async fn start_work_run(
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
pub(crate) async fn approve_flagged_project(
    path: PathBuf,
    state: State<'_, AppState>,
) -> Result<(), String> {
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
pub(crate) async fn skip_work_project(
    path: PathBuf,
    state: State<'_, AppState>,
) -> Result<(), String> {
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
pub(crate) async fn set_work_run_paused(
    paused: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
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
pub(crate) fn clear_work_run(state: State<'_, AppState>) -> Result<(), String> {
    state.work_run_store.set(None)
}

#[tauri::command]
pub(crate) fn work_schedule(state: State<'_, AppState>) -> Result<Option<WorkSchedule>, String> {
    state.work_schedule_store.get()
}

/// Stand up a repeating run of one stored prompt over one set of projects.
///
/// The projects are recorded, not re-derived at firing time: a schedule that
/// re-ran the project filter would quietly change what it targets as
/// repositories are cloned, and the operator would have no way to see it had.
/// The prompt is recorded by *name* for the opposite reason — an edited prompt
/// should take effect, and a deleted one should fail loudly.
#[tauri::command]
pub(crate) async fn set_work_schedule(
    prompt: String,
    projects: Vec<PathBuf>,
    interval_seconds: u64,
    state: State<'_, AppState>,
) -> Result<Option<WorkSchedule>, String> {
    let prompts = state.prompts.clone();
    let store = state.work_schedule_store.clone();
    run_blocking("set_work_schedule", move || {
        if prompts.get(&prompt)?.is_none() {
            return Err(format!("no stored prompt named {prompt}"));
        }
        let schedule = WorkSchedule::new(
            &prompt,
            projects,
            Duration::from_secs(interval_seconds),
            SystemTime::now(),
        )
        .map_err(|error| error.to_string())?;
        store.set(Some(schedule))?;
        store.get()
    })
    .await
}

/// Hold the schedule where it is. It keeps its next-due time, so resuming does
/// not fire for everything that came due while it was held.
#[tauri::command]
pub(crate) fn set_work_schedule_paused(
    paused: bool,
    state: State<'_, AppState>,
) -> Result<Option<WorkSchedule>, String> {
    state
        .work_schedule_store
        .update(|schedule| schedule.paused = paused)?;
    state.work_schedule_store.get()
}

#[tauri::command]
pub(crate) fn clear_work_schedule(state: State<'_, AppState>) -> Result<(), String> {
    state.work_schedule_store.set(None)
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

/// Start the standing schedule's run if one is due, and write down what
/// happened either way.
///
/// Every refusal the on-demand path enforces applies here because this *is*
/// the on-demand path: dirty trees are flagged, admission is the fleet's, the
/// spend ceiling and the expired-credential hold are read from the same
/// snapshot. Nothing here decides whether a project may run.
pub(crate) fn fire_due_schedule(
    client: &DaemonClient,
    work_schedule_store: &work::WorkScheduleStore,
    work_run_store: &work::WorkRunStore,
    prompts: &work::PromptLibrary,
    now: SystemTime,
) {
    let due = match work_schedule_store.get() {
        Ok(Some(schedule)) if schedule.is_due(now) => schedule,
        Ok(_) => return,
        Err(error) => {
            eprintln!("TerminalAI: could not read the work schedule: {error}");
            return;
        }
    };
    // Moved past first, whatever happens next: a firing that failed and left the
    // schedule due would be retried every minute for as long as the cause lasts.
    let missed = match work_schedule_store.update(|schedule| schedule.advance_past(now)) {
        Ok(Some(missed)) => missed,
        Ok(None) => return,
        Err(error) => {
            eprintln!("TerminalAI: could not advance the work schedule: {error}");
            return;
        }
    };
    let result = match scheduled_firing(&due, client, work_run_store, prompts) {
        Ok(result) => result,
        Err(reason) => FiringResult::Skipped { reason },
    };
    if let Err(error) = work_schedule_store.update(|schedule| {
        schedule.record(ScheduleFiring {
            at: now,
            result: result.clone(),
            missed,
        })
    }) {
        eprintln!("TerminalAI: could not record the scheduled run: {error}");
    }
}

/// Why this firing must not start a run yet, if it must not.
///
/// Starting a run replaces the one before it. A schedule that fired while forty
/// projects were still working would destroy the report the operator was going
/// to read, and put a second agent on the first one's uncommitted edits. A
/// finished run is not in the way — it is a report, and the next firing is
/// exactly when replacing it is right.
pub(crate) fn previous_run_blocking(existing: Option<&WorkQueue>) -> Option<String> {
    let existing = existing?;
    (!existing.is_finished()).then(|| "the previous run was still going".to_owned())
}

/// Decide whether this firing may start a run, and start it if so.
fn scheduled_firing(
    schedule: &WorkSchedule,
    client: &DaemonClient,
    work_run_store: &work::WorkRunStore,
    prompts: &work::PromptLibrary,
) -> Result<FiringResult, String> {
    if let Some(reason) = previous_run_blocking(work_run_store.get()?.as_ref()) {
        return Ok(FiringResult::Skipped { reason });
    }
    let projects = schedule.projects.len();
    start_work_run_with(
        schedule.prompt.clone(),
        schedule.projects.clone(),
        client.clone(),
        work_run_store.clone(),
        prompts.clone(),
    )?;
    Ok(FiringResult::Started { projects })
}

pub(crate) fn finish_work_run_session(
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
