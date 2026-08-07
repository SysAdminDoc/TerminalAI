//! Turning an agent's own events into row state.
//!
//! Hooks from the managed hook adapter and events from the Codex app server
//! both land here, are authenticated against the session that owns them, and
//! become one status transition. Split out of `mod.rs` for size; the boundary
//! worth noticing is that nothing in this file may create a row — a hook from an
//! agent this tool did not launch must not fabricate state.

use super::*;

impl SessionRegistry {
    /// Apply a normalized Claude/Codex hook to the matching live session.
    ///
    /// A hook may arrive before the agent has written its native id anywhere
    /// else, so SessionStart first falls back to the newest starting session
    /// for the same agent and working directory. Unknown sessions are ignored:
    /// hooks from agents launched outside TerminalAI must not fabricate rows or
    /// delete state.
    pub fn apply_hook(&self, event: HookEvent) -> bool {
        self.apply_hook_with_token(event, None)
    }

    /// Apply a hook whose per-session secret was carried by the hook adapter.
    ///
    /// The daemon-wide HTTP bearer token only proves that a caller reached the
    /// listener. This second token is the session identity: it is minted when
    /// the row is created and placed only in that agent process's environment.
    pub fn apply_hook_with_token(&self, event: HookEvent, hook_token: Option<&str>) -> bool {
        if let HookSignal::Unknown { event: hook_name } = &event.signal {
            tracing::warn!(
                agent = ?event.agent,
                session_id = ?event.session_id,
                hook_event = %hook_name,
                "unknown agent hook event observed"
            );
        }
        self.apply_hook_from(event, StatusSource::Hook, hook_token)
    }

    /// Re-read the session's branch when it is worth re-reading.
    ///
    /// Returns `Some(value)` to assign — including `Some(None)`, which is how a
    /// session that left a repository stops claiming a branch. `None` means the
    /// existing value stands. Hooks fire once per tool call, so an unconditional
    /// `git rev-parse` here would be a process per tool call across the fleet.
    fn refreshed_branch(&self, id: &SessionId, signal: &HookSignal) -> Option<Option<String>> {
        let (cwd, checked, has_branch) = {
            let state = lock_state(&self.inner);
            let entry = state.entries.get(id)?;
            (
                entry.session.cwd.clone(),
                entry.branch_checked,
                entry.session.branch.is_some(),
            )
        };
        // A branch change is only visible after a tool ran, so a session start
        // and any expired cache are the moments worth spending a process on.
        let due = match checked {
            None => true,
            Some(at) => {
                matches!(signal, HookSignal::SessionStart)
                    || at.elapsed() >= BRANCH_REFRESH_INTERVAL
            }
        };
        if !due {
            return None;
        }
        let branch = crate::review::current_branch(&cwd);
        // Never downgrade a known branch to nothing on a lookup that merely timed
        // out; only report the absence once we have seen it hold.
        if branch.is_none() && has_branch && !matches!(signal, HookSignal::SessionStart) {
            return None;
        }
        Some(branch)
    }

    fn apply_hook_from(
        &self,
        mut event: HookEvent,
        source: StatusSource,
        hook_token: Option<&str>,
    ) -> bool {
        // The pipe transport accepts an already-normalized event and therefore
        // bypasses `parse_hook`; enforce the same boundary for both transports.
        if event
            .session_id
            .as_deref()
            .is_some_and(|session_id| !is_valid_resume_id(session_id))
        {
            event.session_id = None;
        }
        let id = {
            let state = lock_state(&self.inner);
            event
                .session_id
                .as_deref()
                .and_then(|session_id| {
                    state
                        .entries
                        .iter()
                        .find(|(_, entry)| {
                            entry.session.agent == event.agent
                                && (source == StatusSource::AppServer
                                    || (hook_token.is_some()
                                        && !entry.session.hook_token.is_empty()
                                        && hook_token
                                            == Some(entry.session.hook_token.as_str())))
                                && Some(session_id) == entry.session.resume_id.as_deref()
                        })
                        .map(|(id, _)| id.clone())
                })
                .or_else(|| {
                    state
                        .entries
                        .iter()
                        .find(|(_, entry)| {
                            entry.session.agent == event.agent
                                && (source == StatusSource::AppServer
                                    || (hook_token.is_some()
                                        && !entry.session.hook_token.is_empty()
                                        && hook_token
                                            == Some(entry.session.hook_token.as_str())))
                                && event.cwd.as_ref().is_none_or(|cwd| cwd == &entry.session.cwd)
                                && (entry.session.resume_id.is_none()
                                    || event.session_id.is_none())
                                && entry.session.status.is_live()
                        })
                        .map(|(id, _)| id.clone())
                })
        };
        let Some(id) = id else { return false };

        // Resolved before the lock is taken: this shells out to Git.
        let branch = self.refreshed_branch(&id, &event.signal);

        let (session, notifications) = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(&id) else {
                return false;
            };
            let previous_status = entry.session.status;
            let previous_state_since = entry.session.state_since;
            if entry.session.resume_id.is_none()
                && matches!(event.signal, HookSignal::SessionStart)
            {
                if let Some(resume_id) = event.session_id.clone() {
                    entry.session.resume_id = Some(resume_id);
                }
            }
            if let Some(branch) = branch {
                entry.session.branch = branch;
                entry.branch_checked = Some(Instant::now());
            }
            // A plan only arrives on the events that carry one; absence must not
            // erase a plan the session already reported.
            if let Some(progress) = event.progress {
                entry.session.tool_progress = Some(progress);
            }
            match event.signal {
                HookSignal::SessionStart => {
                    // A new run starts with no plan; the previous one is stale.
                    if event.progress.is_none() {
                        entry.session.tool_progress = None;
                    }
                }
                HookSignal::SessionEnd | HookSignal::Stop | HookSignal::StopFailure => {
                    entry.session.set_status_from(SessionStatus::Idle, source)
                }
                HookSignal::UserPromptSubmit
                | HookSignal::PreToolUse
                | HookSignal::SubagentStart => entry
                    .session
                    .set_status_from(SessionStatus::Working, source),
                HookSignal::PostToolUse
                | HookSignal::PostToolUseFailure
                | HookSignal::SubagentStop
                | HookSignal::PostCompact => entry
                    .session
                    .set_status_from(SessionStatus::Thinking, source),
                HookSignal::PermissionRequest | HookSignal::PermissionDenied => entry
                    .session
                    .set_status_from(SessionStatus::NeedsApproval, source),
                HookSignal::PreCompact => entry
                    .session
                    .set_status_from(SessionStatus::Thinking, source),
                HookSignal::Notification { notification } => match notification {
                    HookNotification::PermissionPrompt => entry
                        .session
                        .set_status_from(SessionStatus::NeedsApproval, source),
                    HookNotification::IdlePrompt => entry
                        .session
                        .set_status_from(SessionStatus::AwaitingInput, source),
                    HookNotification::Other => {}
                },
                HookSignal::RateLimited { ref limit } => {
                    let now = SystemTime::now();
                    let reported = RateLimit {
                        scope: limit.scope.clone(),
                        used_percent: limit.used_percent,
                        window_minutes: limit.window_minutes,
                        resets_at: limit
                            .resets_at_unix
                            .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
                            .or_else(|| {
                                limit
                                    .resets_in_seconds
                                    .map(|seconds| now + Duration::from_secs(seconds))
                            }),
                        plan: limit.plan.clone(),
                        reported_at: now,
                    };
                    entry.session.quota = Some(reported.clone());
                    entry.session.rate_limit = Some(reported);
                    entry
                        .session
                        .set_status_from(SessionStatus::RateLimited, source);
                }
                HookSignal::RateLimitCleared { ref limit } => {
                    // Positive evidence the window has room. Keep the reading:
                    // it is the headroom the header warns on, and throwing it
                    // away is why the fleet could only speak about quota after
                    // work had already stopped.
                    let now = SystemTime::now();
                    entry.session.quota = Some(RateLimit {
                        scope: limit.scope.clone(),
                        used_percent: limit.used_percent,
                        window_minutes: limit.window_minutes,
                        resets_at: limit
                            .resets_at_unix
                            .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
                            .or_else(|| {
                                limit
                                    .resets_in_seconds
                                    .map(|seconds| now + Duration::from_secs(seconds))
                            }),
                        plan: limit.plan.clone(),
                        reported_at: now,
                    });
                    // Only move the row off the limited state — a session that
                    // was working is left alone, since a routine quota report
                    // says nothing about it.
                    if entry.session.rate_limit.take().is_some()
                        && entry.session.status == SessionStatus::RateLimited
                    {
                        entry
                            .session
                            .set_status_from(SessionStatus::Thinking, source);
                    }
                }
                HookSignal::Unknown { .. } => {}
            }
            // Any signal at all is proof the provider answered, so a limit whose
            // window has since reset stops holding the row down. Runs after the
            // match so a fresh limit in this same event is not immediately
            // undone by its own predecessor's expiry.
            if !matches!(event.signal, HookSignal::RateLimited { .. }) {
                let now = SystemTime::now();
                if entry
                    .session
                    .rate_limit
                    .as_ref()
                    .is_some_and(|limit| limit.is_expired(now))
                {
                    entry.session.rate_limit = None;
                    if entry.session.status == SessionStatus::RateLimited {
                        entry
                            .session
                            .set_status_from(SessionStatus::Thinking, source);
                    }
                }
            }
            // A non-idle provider signal means the prior composition has been
            // taken up by the agent. Clear the transient guard before the queue
            // reacts to this same status event; an idle row remains guarded so
            // terminal-local echo cannot masquerade as agent output.
            let clear_operator_edit = entry.session.status != SessionStatus::Idle;
            if clear_operator_edit {
                entry.queue.clear_focus_edit();
                entry.session.queued_prompts = entry.queue.len();
                entry.session.queue_paused = entry.queue.paused();
            }
            let session = entry.session.clone();
            let _ = entry;
            if clear_operator_edit {
                state.operator_edited.remove(&id);
            }
            let notifications = state.notifications.observe(
                &session,
                previous_status,
                previous_state_since,
                SystemTime::now(),
            );
            (session, notifications)
        };
        let id = session.id.clone();
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        self.emit_notification_changes(notifications);
        // The queue advances on exactly this signal — the reported status the
        // fleet row is drawn from — rather than on a timer.
        self.pump_queue(&id);
        true
    }

    /// Apply either a legacy hook or an app-server event without making the
    /// daemon's event model choose one transport over the other. App-server
    /// notifications are only accepted for a Codex row that already carries
    /// the matching native thread id; unknown external threads never create a
    /// fleet entry.
    pub fn apply_agent_event(&self, event: AgentEvent) -> bool {
        let matched = match event.clone() {
            AgentEvent::Hook(event) => self.apply_hook(event),
            AgentEvent::AppServer(event) => self.apply_app_server_event(event),
        };
        if matched {
            self.emit(RegistryEvent::AgentEvent { event });
        }
        matched
    }

    fn apply_app_server_event(&self, event: AppServerEvent) -> bool {
        let Some(thread_id) = app_server_thread_id(&event) else {
            return false;
        };
        if !self.has_native_session(crate::agent::Agent::Codex, thread_id) {
            return false;
        }
        let Some(signal) = app_server_signal(&event) else {
            return true;
        };
        self.apply_hook_from(
            HookEvent {
                agent: crate::agent::Agent::Codex,
                session_id: Some(thread_id.to_owned()),
                cwd: None,
                signal,
                // The app-server transport carries plan updates on its own
                // channel; nothing countable rides this event.
                progress: None,
            },
            StatusSource::AppServer,
            None,
        )
    }

    fn has_native_session(&self, agent: crate::agent::Agent, native_id: &str) -> bool {
        let state = lock_state(&self.inner);
        state.entries.values().any(|entry| {
            entry.session.agent == agent && entry.session.resume_id.as_deref() == Some(native_id)
        })
    }
}

fn app_server_thread_id(event: &AppServerEvent) -> Option<&str> {
    match event {
        AppServerEvent::ThreadStatusChanged { thread_id, .. }
        | AppServerEvent::TokenUsageUpdated { thread_id, .. }
        | AppServerEvent::ApprovalRequested { thread_id, .. } => Some(thread_id),
        AppServerEvent::Unknown { .. } => None,
    }
}

fn app_server_signal(event: &AppServerEvent) -> Option<HookSignal> {
    match event {
        AppServerEvent::ThreadStatusChanged { status, .. } => {
            if status
                .active_flags
                .iter()
                .any(|flag| is_approval_flag(flag))
            {
                Some(HookSignal::Notification {
                    notification: HookNotification::PermissionPrompt,
                })
            } else if status.active_flags.iter().any(|flag| is_input_flag(flag)) {
                Some(HookSignal::Notification {
                    notification: HookNotification::IdlePrompt,
                })
            } else {
                match event_status_key(&status.kind) {
                    Some("active") => Some(HookSignal::PreToolUse),
                    Some("idle") | Some("notloaded") | Some("systemerror") => {
                        Some(HookSignal::Stop)
                    }
                    _ => None,
                }
            }
        }
        AppServerEvent::ApprovalRequested { .. } => Some(HookSignal::Notification {
            notification: HookNotification::PermissionPrompt,
        }),
        AppServerEvent::TokenUsageUpdated { .. } | AppServerEvent::Unknown { .. } => None,
    }
}

fn event_status_key(value: &str) -> Option<&str> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "active" => Some("active"),
        "idle" => Some("idle"),
        "notloaded" | "not_loaded" => Some("notloaded"),
        "systemerror" | "system_error" => Some("systemerror"),
        _ => None,
    }
}

fn is_approval_flag(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("approval") || value.contains("permission")
}

fn is_input_flag(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("input") || value.contains("user")
}
