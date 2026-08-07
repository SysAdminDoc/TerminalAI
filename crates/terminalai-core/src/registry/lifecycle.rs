//! Bringing a session up, keeping it up, and taking it down.
//!
//! The supervision half of the registry: spawning the process, watching it,
//! deciding what one exit means, running the operator's teardown, scheduling the
//! restart, and removing the row. Split out of `mod.rs` for size — these are
//! `SessionRegistry` methods reaching the same private state as before.
//!
//! The policy these methods apply is not theirs: whether an exit earns a restart
//! and how long to wait is [`crate::restart`]'s, and whether anything new may
//! start is [`crate::admission`]'s. What lives here is the ordering, the
//! threads, and the generation checks that keep a stale callback from acting on
//! a row that has already moved on.

use super::*;

impl SessionRegistry {
    pub(super) fn start_entry(
        &self,
        id: &SessionId,
        command: ResolvedCommand,
        generation: u64,
    ) -> Result<(), RegistryError> {
        let registry = self.clone();
        let worker_id = id.clone();
        let span = self.span_for(id);
        thread::Builder::new()
            .name(format!("terminalai-environment-{id}"))
            .spawn(move || {
                let _entered = span.enter();
                if let Err(error) = registry.prepare_and_start(&worker_id, &command, generation) {
                    registry.finish_start_failure(&worker_id, generation, error);
                }
            })
            .map(|_| ())
            .map_err(|error| RegistryError::WorkerSpawn {
                phase: "setup",
                cause: error.to_string(),
            })
    }

    fn prepare_and_start(
        &self,
        id: &SessionId,
        command: &ResolvedCommand,
        generation: u64,
    ) -> Result<(), RegistryError> {
        // Before anything reads the working directory: a session that asked for
        // its own checkout must have one by the time the lease copies config
        // into it and the agent is told where to run.
        self.provision_worktree(id)?;
        let (cwd, environment_spec, ports, hook_token, launch_environment) =
            self.runtime_environment(id)?;
        let command = &ResolvedCommand {
            cwd: cwd.clone(),
            ..command.clone()
        };
        tracing::debug!("preparing session environment");
        let mut environment = environment::variables(&id.0, &ports);
        environment.push(("TERMINALAI_HOOK_TOKEN".into(), hook_token));
        // The launch's own additions — the agent's config directory and any
        // parent variables the operator named. Applied before the lease and the
        // setup hook so both see the account this session is actually running
        // as, and after the supervisor's own variables so a passthrough name
        // cannot displace one (`agent_environment` refuses TERMINALAI_* anyway).
        environment.extend(launch_environment);

        // The repository's own lease, applied before the raw setup hook so the
        // hook sees the copied config, the compose project name and the session
        // database URL it is meant to build on.
        let lease = match lease::Lease::load(&cwd) {
            Ok(lease) => lease,
            Err(error) => {
                // A lease the operator wrote but that cannot be read is refused,
                // not ignored: starting anyway would produce a session that
                // looks isolated and shares a database.
                return Err(RegistryError::Environment(EnvironmentError::HookSpawn {
                    phase: "lease",
                    cause: error.to_string(),
                }));
            }
        };
        let resolved_lease = match lease.as_ref() {
            Some(lease) => Some(lease.resolve(&id.0, &cwd).map_err(|error| {
                RegistryError::Environment(EnvironmentError::HookSpawn {
                    phase: "lease",
                    cause: error.to_string(),
                })
            })?),
            None => None,
        };
        if let Some(resolved) = &resolved_lease {
            if let Err(error) = self.apply_lease(id, resolved, &cwd, &mut environment) {
                self.report_failed_teardown(
                    id,
                    environment::run_teardown(&environment_spec, &id.0, &cwd, &ports),
                );
                return Err(error);
            }
        }

        if let Err(error) = environment::run_setup(&environment_spec, &id.0, &cwd, &ports) {
            self.report_failed_teardown(id, environment::run_teardown(&environment_spec, &id.0, &cwd, &ports));
            return Err(error.into());
        }
        if !self.start_is_current(id, generation) {
            self.report_failed_teardown(id, environment::run_teardown(&environment_spec, &id.0, &cwd, &ports));
            return Err(RegistryError::NotRunning(id.clone()));
        }
        let pty = match self.spawn_pty(id, command, generation, &environment) {
            Ok(pty) => pty,
            Err(error) => {
                self.report_failed_teardown(id, environment::run_teardown(&environment_spec, &id.0, &cwd, &ports));
                return Err(error);
            }
        };
        let accepted = {
            let mut state = lock_state(&self.inner);
            match state.entries.get_mut(id) {
                Some(entry)
                    if entry.generation == generation
                        && entry.pty.is_none()
                        && !entry.stop_requested
                        && entry.session.status == SessionStatus::Starting =>
                {
                    entry.session.mark_spawned_at(pty.pid(), SystemTime::now());
                    entry.pty = Some(pty.clone());
                    entry.teardown_done = false;
                    true
                }
                _ => false,
            }
        };
        if !accepted {
            let _ = pty.kill();
            self.report_failed_teardown(id, environment::run_teardown(&environment_spec, &id.0, &cwd, &ports));
            return Err(RegistryError::NotRunning(id.clone()));
        }
        let background = {
            let state = lock_state(&self.inner);
            state
                .entries
                .get(id)
                .is_some_and(|entry| state.focused.as_ref() != Some(id) && !entry.session.pinned)
        };
        if let Err(error) = pty.set_background(background) {
            tracing::warn!(
                session_id = %id,
                background,
                error = %error,
                "could not apply process background policy"
            );
        }
        tracing::info!(pid = ?pty.pid(), "session process started");
        self.emit_session(id);
        self.spawn_monitor(id.clone(), pty, generation, self.span_for(id));
        Ok(())
    }

    fn finish_start_failure(&self, id: &SessionId, generation: u64, error: RegistryError) {
        let span = self.span_for(id);
        tracing::error!(parent: &span, error = %error, "session start failed");
        let (session, notifications) = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation
                || entry.stop_requested
                || entry.pty.is_some()
                || entry.session.phase != SessionPhase::Preparing
            {
                return;
            }
            let previous_status = entry.session.status;
            let previous_state_since = entry.session.state_since;
            let now = SystemTime::now();
            entry
                .session
                .mark_resurrectable_at_from(None, now, StatusSource::Supervisor);
            entry.session.phase = SessionPhase::Failed;
            entry.session.health = crate::session::SessionHealth::Failed;
            entry
                .session
                .set_last_line(&format!("Environment startup failed: {error}"));
            let session = entry.session.clone();
            let notifications =
                state
                    .notifications
                    .observe(&session, previous_status, previous_state_since, now);
            (session, notifications)
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        self.emit_notification_changes(notifications);
        self.drain_queue();
    }

    fn start_is_current(&self, id: &SessionId, generation: u64) -> bool {
        let state = lock_state(&self.inner);
        state.entries.get(id).is_some_and(|entry| {
            entry.generation == generation
                && !entry.stop_requested
                && entry.session.phase == SessionPhase::Preparing
        })
    }



    fn runtime_environment(
        &self,
        id: &SessionId,
    ) -> Result<RuntimeEnvironment, RegistryError> {
        let state = lock_state(&self.inner);
        let entry = state
            .entries
            .get(id)
            .ok_or_else(|| RegistryError::Missing(id.clone()))?;
        Ok((
            entry.session.cwd.clone(),
            entry.spec.environment.clone(),
            entry.session.ports.clone(),
            entry.session.hook_token.clone(),
            // Spec-derived, so a launch that names a config directory or a
            // parent variable is honoured on every restart of that session and
            // not only on its first spawn.
            entry.spec.agent_environment()?,
        ))
    }

    pub(super) fn drain_queue(&self) {
        loop {
            let (id, command, generation) = {
                let mut state = lock_state(&self.inner);
                if admission_block(&state).is_some() {
                    return;
                }
                let Some(id) = state.queue.pop_front() else {
                    return;
                };
                let Some(entry) = state.entries.get_mut(&id) else {
                    continue;
                };
                if entry.session.status != SessionStatus::Queued {
                    continue;
                }
                entry.generation = entry.generation.saturating_add(1);
                entry
                    .session
                    .begin_restart_at_from(SystemTime::now(), StatusSource::Admission);
                entry.session.mark_preparing();
                (id, entry.command.clone(), entry.generation)
            };

            self.emit_session(&id);
            if self.start_entry(&id, command, generation).is_err() {
                let session = {
                    let mut state = lock_state(&self.inner);
                    let Some(entry) = state.entries.get_mut(&id) else {
                        continue;
                    };
                    if entry.generation != generation {
                        continue;
                    }
                    entry.session.mark_resurrectable_at_from(
                        None,
                        SystemTime::now(),
                        StatusSource::Supervisor,
                    );
                    entry.session.clone()
                };
                self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
            }
        }
    }

    pub(super) fn remove_entry(&self, id: &SessionId) {
        let worktree_failures = self.release_worktree(id);
        if !worktree_failures.is_empty() {
            tracing::warn!(session = %id, ?worktree_failures, "session worktree was not fully removed");
        }
        let (removed, notifications) = {
            let mut state = lock_state(&self.inner);
            if state.focused.as_ref() == Some(id) {
                state.focused = None;
            }
            state.operator_edited.remove(id);
            let removed = state.entries.remove(id).is_some();
            let notifications = if removed {
                state.notifications.retract_session(id)
            } else {
                Vec::new()
            };
            (removed, notifications)
        };
        if removed {
            spool_forget(&self.inner, id);
            self.emit(RegistryEvent::SessionRemoved { id: id.clone() });
        }
        self.emit_notification_changes(notifications);
    }

    pub(super) fn mark_process_exit(&self, id: &SessionId, generation: u64, exit_code: Option<u32>) {
        let (restart, session, notifications, teardown) = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            let previous_status = entry.session.status;
            let previous_state_since = entry.session.state_since;
            tracing::info!(
                parent: &entry.span,
                exit_code = ?exit_code,
                "session process exited"
            );
            entry.pty = None;
            entry.generation = entry.generation.saturating_add(1);
            let now = SystemTime::now();
            let restart = if entry.stop_requested {
                entry.stop_requested = false;
                entry
                    .session
                    .mark_resurrectable_at_from(exit_code, now, StatusSource::ProcessExit);
                None
            } else {
                match entry.session.schedule_restart_at_from(
                    exit_code,
                    now,
                    StatusSource::ProcessExit,
                ) {
                    RestartDecision::Backoff(delay) => Some((entry.generation, delay)),
                    RestartDecision::Failed | RestartDecision::Finished => None,
                }
            };
            let teardown = if entry.teardown_done {
                None
            } else {
                entry.teardown_done = true;
                let final_phase = entry.session.phase;
                entry.session.mark_tearing_down();
                Some(TeardownTask {
                    id: id.clone(),
                    generation: entry.generation,
                    final_phase,
                    restart,
                    cwd: entry.session.cwd.clone(),
                    spec: entry.spec.environment.clone(),
                    ports: entry.session.ports.clone(),
                    span: entry.span.clone(),
                })
            };
            let session = entry.session.clone();
            let notifications =
                state
                    .notifications
                    .observe(&session, previous_status, previous_state_since, now);
            (restart, session, notifications, teardown)
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        self.emit_notification_changes(notifications);
        // An exit pauses the queue rather than losing it: those prompts are
        // still what the operator wanted done, and reviving the session should
        // not mean retyping them.
        self.pump_queue(id);
        if let Some(task) = teardown {
            self.spawn_teardown(task);
        } else {
            self.complete_process_exit(id, restart);
        }
    }

    fn spawn_teardown(&self, task: TeardownTask) {
        let TeardownTask {
            id,
            generation,
            final_phase,
            restart,
            cwd,
            spec,
            ports,
            span,
        } = task;
        let registry = self.clone();
        let worker_id = id.clone();
        let worker_cwd = cwd.clone();
        let worker_spec = spec.clone();
        let worker_ports = ports.clone();
        let result = thread::Builder::new()
            .name(format!("terminalai-environment-teardown-{id}"))
            .spawn(move || {
                let _entered = span.enter();
                // The repository's lease is released first, so the operator's
                // own teardown script runs against a tree whose containers and
                // session database are already gone.
                let mut failures = registry.release_lease(&worker_id, &worker_cwd);
                if let Err(error) = environment::run_teardown(
                    &worker_spec,
                    &worker_id.0,
                    &worker_cwd,
                    &worker_ports,
                ) {
                    failures.push(error.to_string());
                }
                // Every failure is reported, not just the first: a compose stack
                // left running matters even when the database also failed to
                // drop, and the operator has to clean up both.
                let error = (!failures.is_empty()).then(|| failures.join("; "));
                registry.finish_teardown(&worker_id, generation, final_phase, restart, error);
            });
        if let Err(error) = result {
            self.finish_teardown(
                &id,
                generation,
                final_phase,
                restart,
                Some(format!("could not start teardown worker: {error}")),
            );
        }
    }

    /// Record a teardown that failed while unwinding a launch.
    ///
    /// These run after setup already did something — created a database,
    /// started containers, wrote files — so a failure here means leaked
    /// resources. It was previously discarded with `let _`, which made a leak
    /// indistinguishable from a clean unwind; the launch error itself is still
    /// what the caller returns, because that is the reason the session is gone.
    fn report_failed_teardown(&self, id: &SessionId, result: Result<(), EnvironmentError>) {
        let Err(error) = result else { return };
        tracing::warn!(
            session = %id,
            "environment teardown failed while unwinding a launch; resources may be leaked: {error}"
        );
        self.emit(RegistryEvent::Log {
            entry: LogEntry {
                at: SystemTime::now(),
                level: "WARN".to_owned(),
                target: "terminalai::environment".to_owned(),
                message: "environment teardown failed while unwinding a launch; resources may be leaked"
                    .to_owned(),
                fields: BTreeMap::from([
                    ("session".to_owned(), id.0.clone()),
                    ("error".to_owned(), error.to_string()),
                ]),
            },
        });
    }

    fn finish_teardown(
        &self,
        id: &SessionId,
        generation: u64,
        final_phase: SessionPhase,
        restart: Option<(u64, Duration)>,
        error: Option<String>,
    ) {
        if let Some(error) = error.as_deref() {
            let span = self.span_for(id);
            tracing::error!(parent: &span, error, "session teardown failed");
        }
        let session = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation || entry.session.phase != SessionPhase::TearingDown {
                return;
            }
            entry.session.phase = final_phase;
            if let Some(error) = error {
                entry
                    .session
                    .set_last_line(&format!("Environment teardown failed: {error}"));
            }
            entry.session.clone()
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        self.complete_process_exit(id, restart);
    }

    fn complete_process_exit(&self, id: &SessionId, restart: Option<(u64, Duration)>) {
        self.drain_queue();
        if let Some((generation, delay)) = restart {
            self.schedule_restart(id.clone(), generation, delay);
        }
    }

    pub(super) fn mark_process_unknown(&self, id: &SessionId, generation: u64) {
        let session = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            if entry.session.status == SessionStatus::Unknown {
                return;
            }
            tracing::warn!(parent: &entry.span, "session process state is unknown");
            entry.session.mark_unknown_at(SystemTime::now());
            entry.session.clone()
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
    }

    pub(super) fn schedule_restart(&self, id: SessionId, generation: u64, delay: Duration) {
        let sequence = self.inner.restart_sequence.fetch_add(1, Ordering::Relaxed);
        let task_id = id.clone();
        if self
            .inner
            .restart_tx
            .send(RestartTask {
                due: Instant::now() + delay,
                sequence,
                id,
                generation,
            })
            .is_err()
        {
            self.mark_restart_failed(&task_id, generation);
        }
    }

    fn restart(&self, id: SessionId, pending_generation: u64) {
        let (command, generation) = {
            let mut state = lock_state(&self.inner);
            let admission_full = admission_block(&state).is_some();
            let Some(entry) = state.entries.get_mut(&id) else {
                return;
            };
            if entry.generation != pending_generation
                || entry.pty.is_some()
                || entry.stop_requested
                || entry.session.phase != SessionPhase::Backoff
            {
                return;
            }
            if admission_full {
                drop(state);
                self.restart_spawn_failed(&id, pending_generation);
                return;
            }
            entry.generation = entry.generation.saturating_add(1);
            let generation = entry.generation;
            entry.session.begin_restart_at(SystemTime::now());
            entry.session.mark_preparing();
            (entry.command.clone(), generation)
        };
        self.emit_session(&id);
        if self.start_entry(&id, command, generation).is_err() {
            self.restart_spawn_failed(&id, generation);
        }
    }

    fn restart_spawn_failed(&self, id: &SessionId, generation: u64) {
        let (restart, session, notifications) = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            let previous_status = entry.session.status;
            let previous_state_since = entry.session.state_since;
            let now = SystemTime::now();
            entry.generation = entry.generation.saturating_add(1);
            let restart =
                match entry
                    .session
                    .schedule_restart_at_from(None, now, StatusSource::Supervisor)
                {
                    RestartDecision::Backoff(delay) => Some((entry.generation, delay)),
                    RestartDecision::Failed | RestartDecision::Finished => None,
                };
            let session = entry.session.clone();
            let notifications =
                state
                    .notifications
                    .observe(&session, previous_status, previous_state_since, now);
            (restart, session, notifications)
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        self.emit_notification_changes(notifications);
        if let Some((generation, delay)) = restart {
            self.schedule_restart(id.clone(), generation, delay);
        }
    }

    fn mark_restart_failed(&self, id: &SessionId, generation: u64) {
        let (session, notifications) = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation
                || entry.pty.is_some()
                || entry.stop_requested
                || entry.session.phase != SessionPhase::Backoff
            {
                return;
            }
            let previous_status = entry.session.status;
            let previous_state_since = entry.session.state_since;
            let now = SystemTime::now();
            entry
                .session
                .mark_resurrectable_at_from(None, now, StatusSource::Supervisor);
            entry.session.phase = SessionPhase::Failed;
            entry.session.health = crate::session::SessionHealth::Failed;
            entry.generation = entry.generation.saturating_add(1);
            let session = entry.session.clone();
            let notifications =
                state
                    .notifications
                    .observe(&session, previous_status, previous_state_since, now);
            (session, notifications)
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        self.emit_notification_changes(notifications);
    }

    /// Watch one session for exit.
    ///
    /// Blocks on the child's own exit signal rather than asking twenty times a
    /// second whether it is still alive — at the thirty-session target that
    /// polling was 600 wakeups per second doing nothing. Polling remains only as
    /// the fallback for platforms and failure paths that expose no waitable
    /// handle, and there it runs at the slower error cadence.
    fn spawn_monitor(
        &self,
        id: SessionId,
        session: Arc<dyn AgentSession>,
        generation: u64,
        span: tracing::Span,
    ) {
        let registry = self.clone();
        let _ = thread::Builder::new()
            .name(format!("terminalai-session-{id}"))
            .spawn(move || {
                let _entered = span.enter();
                if let Ok(status) = session.wait_for_exit() {
                    registry.mark_process_exit(&id, generation, Some(status));
                    return;
                }
                Self::poll_until_exit(&registry, &id, session.as_ref(), generation);
            });
    }

    fn poll_until_exit(
        registry: &SessionRegistry,
        id: &SessionId,
        session: &dyn AgentSession,
        generation: u64,
    ) {
        loop {
            match session.try_wait() {
                Ok(Some(status)) => {
                    registry.mark_process_exit(id, generation, Some(status));
                    return;
                }
                Err(_) => {
                    registry.mark_process_unknown(id, generation);
                    thread::sleep(Duration::from_millis(250));
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

pub(super) fn restart_scheduler_loop(receiver: Receiver<RestartTask>, inner: Weak<Inner>) {
    let mut pending: BinaryHeap<RestartTask> = BinaryHeap::new();
    loop {
        while pending
            .peek()
            .map(|task| task.due <= Instant::now())
            .unwrap_or(false)
        {
            let task = pending.pop().expect("restart task disappeared");
            let Some(inner) = inner.upgrade() else {
                return;
            };
            SessionRegistry { inner }.restart(task.id, task.generation);
        }

        let wait = pending
            .peek()
            .map(|task| task.due.saturating_duration_since(Instant::now()))
            .unwrap_or(NOTIFICATION_RECHECK_INTERVAL)
            .min(NOTIFICATION_RECHECK_INTERVAL);
        match receiver.recv_timeout(wait) {
            Ok(task) => pending.push(task),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                SessionRegistry { inner }.recheck_notifications(SystemTime::now());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentBinary, Origin};
    
    use crate::launch::spec_for;
    
    use std::path::{Path, PathBuf};
    
    use std::time::Duration;

    #[cfg(windows)]
    #[test]
    fn slow_setup_runs_off_launch_path_and_exposes_preparing_phase() {
        let mut spec = LaunchSpec {
            cwd: std::env::current_dir().expect("test cwd"),
            extra_args: vec!["/c".into(), "exit".into(), "0".into()],
            ..LaunchSpec::default()
        };
        spec.environment.setup = Some("ping -n 5 127.0.0.1 > nul".into());
        let registry = SessionRegistry::new();
        let started = std::time::Instant::now();
        let id = registry
            .launch(
                spec,
                AgentBinary {
                    agent: Agent::Claude,
                    path: PathBuf::from("cmd.exe"),
                    origin: Origin::Path,
                },
            )
            .expect("launch with slow setup hook");
        // Derived from the hook, not picked: `ping -n 5` sleeps about four
        // seconds, so returning inside three proves `launch` did not wait for
        // it. The bound used to be one second, which proves the same thing on an
        // idle machine and nothing at all on a busy one — it failed three times
        // during release bumps, where the suite runs while cargo is still
        // compiling. The load-independent proof is the phase assertion below:
        // had `launch` waited, setup would have finished and the row would not
        // still read `Preparing`.
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "launch waited for the setup worker"
        );
        assert_eq!(
            registry
                .snapshot()
                .into_iter()
                .find(|session| session.id == id)
                .expect("preparing row")
                .phase,
            SessionPhase::Preparing
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            let row = registry
                .snapshot()
                .into_iter()
                .find(|session| session.id == id)
                .expect("session remains tracked");
            if row.pid.is_some() || row.status == SessionStatus::Exited {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        registry.shutdown();
    }

    #[test]
    fn admission_blocked_restart_consumes_budget_without_spawning_retry_threads() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(1, None));
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let active_id = SessionId::new(1);
        let target_id = SessionId::new(2);
        let mut active = Session::new(active_id.clone(), &spec);
        active.set_status(SessionStatus::Idle);
        let mut target = Session::new(target_id.clone(), &spec);
        target.status = SessionStatus::Exited;
        target.phase = SessionPhase::Backoff;
        target.health = crate::session::SessionHealth::Degraded;
        target.restarts = 1;
        target.backoff_until = Some(SystemTime::UNIX_EPOCH);
        {
            let mut state = lock_state(&registry.inner);
            state.entries.insert(
                active_id,
                Entry {
                    session: active,
                    command: ResolvedCommand {
                        program: PathBuf::from("active-agent.exe"),
                        args: Vec::new(),
                        cwd: cwd.clone(),
                    },
                    spec: spec.clone(),
                    pty: None,
                    scrollback: RingBuffer::default(),
                    grid: TerminalGrid::default(),
                    queue: crate::queue::PromptQueue::default(),
                    generation: 1,
                    stop_requested: false,
                    branch_checked: None,
                    teardown_done: true,
                    span: tracing::Span::none(),
                },
            );
            state.entries.insert(
                target_id.clone(),
                Entry {
                    session: target,
                    command: ResolvedCommand {
                        program: PathBuf::from("retry-agent.exe"),
                        args: Vec::new(),
                        cwd,
                    },
                    spec,
                    pty: None,
                    scrollback: RingBuffer::default(),
                    grid: TerminalGrid::default(),
                    queue: crate::queue::PromptQueue::default(),
                    generation: 1,
                    stop_requested: false,
                    branch_checked: None,
                    teardown_done: true,
                    span: tracing::Span::none(),
                },
            );
        }

        registry.schedule_restart(target_id.clone(), 1, Duration::ZERO);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let row = registry
                .snapshot()
                .into_iter()
                .find(|session| session.id == target_id)
                .expect("restart row");
            if row.restarts >= 2 {
                assert_eq!(row.phase, SessionPhase::Backoff);
                assert!(row.backoff_until.is_some());
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        panic!("admission-blocked restart did not consume its budget");
    }

    /// The exit code was captured and never consulted, so an agent that finished
    /// its work was brought back up to five times, billing quota each time.
    #[test]
    fn a_session_that_exits_cleanly_stays_stopped() {
        for (exit_code, expected_phase, expected_restarts) in [
            (0u32, SessionPhase::Finished, 0u32),
            (crate::session::STATUS_CONTROL_C_EXIT, SessionPhase::Finished, 0),
            (1, SessionPhase::Backoff, 1),
        ] {
            let registry = SessionRegistry::new();
            let cwd = Path::new(".").to_path_buf();
            let spec = spec_for(Agent::Claude, &cwd);
            let id = SessionId::new(1);
            let mut session = Session::new(id.clone(), &spec);
            session.set_status(SessionStatus::Working);
            {
                let mut state = lock_state(&registry.inner);
                state.entries.insert(
                    id.clone(),
                    Entry {
                        session,
                        command: ResolvedCommand {
                            program: PathBuf::from("agent.exe"),
                            args: Vec::new(),
                            cwd: cwd.clone(),
                        },
                        spec: spec.clone(),
                        pty: None,
                        scrollback: RingBuffer::default(),
                        grid: TerminalGrid::default(),
                        queue: crate::queue::PromptQueue::default(),
                        generation: 1,
                        stop_requested: false,
                        branch_checked: None,
                        teardown_done: true,
                        span: tracing::Span::none(),
                    },
                );
            }

            registry.mark_process_exit(&id, 1, Some(exit_code));

            let row = registry
                .snapshot()
                .into_iter()
                .find(|session| session.id == id)
                .expect("row survives its process");
            assert_eq!(row.status, SessionStatus::Exited);
            assert_eq!(
                row.phase, expected_phase,
                "exit code {exit_code:#x} took the wrong branch"
            );
            assert_eq!(row.restarts, expected_restarts);
            assert_eq!(row.last_exit_code, Some(exit_code));
            if expected_phase == SessionPhase::Finished {
                assert_eq!(row.backoff_until, None, "a finished session is scheduled");
            } else {
                assert!(row.backoff_until.is_some(), "a crash was not rescheduled");
            }
            registry.shutdown();
        }
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_runs_teardown_for_active_sessions() {
        let marker = std::env::temp_dir().join(format!(
            "terminalai-teardown-{}-{}.txt",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&marker);
        let mut spec = spec_for(Agent::Claude, &std::env::current_dir().expect("test cwd"));
        spec.extra_args = vec!["/c".into(), "ping -n 30 127.0.0.1 > nul".into()];
        spec.environment.teardown = Some(format!(
            "echo %TERMINALAI_SESSION_ID% %TERMINALAI_PORTS% > {}",
            marker.display()
        ));
        let registry = SessionRegistry::new();
        let id = registry
            .launch(
                spec,
                AgentBinary {
                    agent: Agent::Claude,
                    path: PathBuf::from("cmd.exe"),
                    origin: Origin::Path,
                },
            )
            .expect("spawn teardown test process");

        registry.shutdown();
        // Wait for content, not for existence: `echo > file` creates the file
        // before it writes a byte into it, so a machine under load reads an
        // empty marker and reports a teardown that in fact ran correctly.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut teardown = String::new();
        while std::time::Instant::now() < deadline {
            teardown = std::fs::read_to_string(&marker).unwrap_or_default();
            if !teardown.trim().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!teardown.trim().is_empty(), "teardown marker never written");
        assert!(
            teardown.contains(&id.0),
            "teardown omitted session id: {teardown:?}"
        );
        assert!(
            teardown.contains("42000,42001,42002,42003"),
            "teardown omitted port block: {teardown:?}"
        );
        let _ = std::fs::remove_file(marker);
    }

    #[cfg(windows)]
    #[test]
    fn exited_process_enters_backoff_and_restarts_with_new_supervision_state() {
        use std::time::Instant;

        let registry = SessionRegistry::new();
        let spec = LaunchSpec {
            agent: Agent::Claude,
            cwd: std::env::current_dir().expect("test cwd"),
            extra_args: vec!["/c".into(), "exit".into(), "7".into()],
            ..LaunchSpec::default()
        };
        let id = registry
            .launch(
                spec,
                AgentBinary {
                    agent: Agent::Claude,
                    path: PathBuf::from("cmd.exe"),
                    origin: Origin::Path,
                },
            )
            .expect("spawn short-lived process");
        let launch_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < launch_deadline && registry.snapshot()[0].pid.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(registry.snapshot()[0].pid.is_some());

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_backoff = false;
        while Instant::now() < deadline {
            let session = registry
                .snapshot()
                .into_iter()
                .find(|session| session.id == id)
                .expect("session remains tracked");
            if session.restarts >= 1 {
                saw_backoff |= session.phase == SessionPhase::Backoff;
                assert_eq!(session.last_exit_code, Some(7));
            }
            if session.restarts >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let session = registry
            .snapshot()
            .into_iter()
            .find(|session| session.id == id)
            .expect("session remains tracked");
        assert!(saw_backoff, "unexpected supervision state: {session:?}");
        assert!(session.restarts >= 2, "restart did not occur: {session:?}");
        registry.shutdown();
    }

    #[test]
    fn poisoned_state_lock_is_recovered_without_panicking() {
        let registry = SessionRegistry::new();
        let poisoned = registry.clone();
        std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _state = lock_state(&poisoned.inner);
                panic!("poison registry test lock");
            }));
        })
        .join()
        .expect("poisoning thread");

        assert!(registry.is_poisoned());
        assert!(registry.snapshot().is_empty());
        assert_eq!(registry.admission_snapshot().live_sessions, 0);
    }
}
