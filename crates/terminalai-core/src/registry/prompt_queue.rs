//! The per-session prompt queue, and delivering one message to many sessions.
//!
//! Split out of `mod.rs` for size: these are `SessionRegistry` methods like any
//! other and reach the same private state, but they are the one group that is
//! about what an operator has asked for rather than about what a process is
//! doing.

use super::*;

impl SessionRegistry {
    /// Add a prompt to a session's queue.
    ///
    /// Fires immediately when the session is already idle, which is what makes
    /// the queue usable as "and then do this" rather than only as a backlog.
    pub fn enqueue_prompt(&self, id: &SessionId, text: &str) -> Result<u64, RegistryError> {
        let queued = self.with_queue(id, |queue| queue.push(text))?;
        self.emit_session(id);
        self.pump_queue(id);
        Ok(queued)
    }

    /// Replace a queued prompt that has not fired yet.
    pub fn edit_queued_prompt(
        &self,
        id: &SessionId,
        prompt: u64,
        text: &str,
    ) -> Result<(), RegistryError> {
        self.with_queue(id, |queue| queue.edit(prompt, text))?;
        self.emit_session(id);
        Ok(())
    }

    /// Withdraw a queued prompt before it fires.
    pub fn remove_queued_prompt(&self, id: &SessionId, prompt: u64) -> Result<(), RegistryError> {
        self.with_queue(id, |queue| queue.remove(prompt))?;
        self.emit_session(id);
        Ok(())
    }

    /// Move a queued prompt to a new position.
    pub fn reorder_queued_prompt(
        &self,
        id: &SessionId,
        prompt: u64,
        to: usize,
    ) -> Result<(), RegistryError> {
        self.with_queue(id, |queue| queue.reorder(prompt, to))?;
        self.emit_session(id);
        Ok(())
    }

    /// Stop a queue advancing on its own.
    pub fn pause_queue(&self, id: &SessionId) -> Result<(), RegistryError> {
        self.with_queue(id, |queue| {
            queue.pause(crate::queue::PauseReason::Operator);
            Ok(())
        })?;
        self.emit_session(id);
        Ok(())
    }

    /// Resume a paused queue, sending the next prompt if the session is ready.
    pub fn resume_queue(&self, id: &SessionId) -> Result<(), RegistryError> {
        self.with_queue(id, |queue| {
            queue.resume();
            Ok(())
        })?;
        self.emit_session(id);
        self.pump_queue(id);
        Ok(())
    }

    /// The prompts waiting on one session.
    ///
    /// Fetched on demand rather than carried on every `Session`, which is
    /// cloned on each status change: 32 prompts of up to a quarter of a
    /// megabyte would make that the most expensive thing the fleet does.
    pub fn queued_prompts(
        &self,
        id: &SessionId,
    ) -> Result<Vec<crate::queue::QueuedPrompt>, RegistryError> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .map(|entry| entry.queue.entries().iter().cloned().collect())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    fn with_queue<T>(
        &self,
        id: &SessionId,
        edit: impl FnOnce(&mut crate::queue::PromptQueue) -> Result<T, crate::queue::QueueError>,
    ) -> Result<T, RegistryError> {
        let mut state = lock_state(&self.inner);
        let entry = state
            .entries
            .get_mut(id)
            .ok_or_else(|| RegistryError::Missing(id.clone()))?;
        let result = edit(&mut entry.queue).map_err(RegistryError::Queue)?;
        entry.session.queued_prompts = entry.queue.len();
        entry.session.queue_paused = entry.queue.paused();
        Ok(result)
    }

    /// Let a session's queue react to its current status.
    ///
    /// Called wherever a status can change, because the queue advances on the
    /// same reported signal the fleet row is drawn from — never on a timer, and
    /// never on evidence the operator cannot also see.
    pub(super) fn pump_queue(&self, id: &SessionId) {
        let send = {
            let mut state = lock_state(&self.inner);
            let focused_and_edited =
                state.focused.as_ref() == Some(id) && state.operator_edited.contains(id);
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if focused_and_edited {
                entry.queue.hold_for_focus_edit();
            } else {
                entry.queue.clear_focus_edit();
            }
            entry.session.queued_prompts = entry.queue.len();
            entry.session.queue_paused = entry.queue.paused();
            // A session with no queue is the common case; do nothing and touch
            // nothing, so this can be called freely on every status change.
            if entry.queue.is_empty() && entry.queue.paused().is_none() {
                return;
            }
            let status = entry.session.status;
            let action = entry.queue.observe(status);
            entry.session.queued_prompts = entry.queue.len();
            entry.session.queue_paused = entry.queue.paused();
            match action {
                crate::queue::QueueAction::Idle => None,
                crate::queue::QueueAction::Send(text) => Some(text),
            }
        };
        let Some(text) = send else {
            self.emit_session(id);
            return;
        };
        // The same bracketed-paste framing a typed reply uses. Without it a
        // multi-line prompt is submitted a line at a time and the agent acts on
        // the first fragment.
        let payload = format!("\u{1b}[200~{text}\u{1b}[201~\r");
        if let Err(error) = self.write(id, payload.as_bytes()) {
            // The prompt has already left the queue. Say so rather than
            // silently dropping what the operator asked for.
            tracing::warn!(session = %id, %error, "a queued prompt could not be delivered");
            let _ = self.with_queue(id, |queue| {
                queue.pause(crate::queue::PauseReason::NotRunning);
                Ok(())
            });
        }
        self.emit_session(id);
    }

    /// Send the same bytes to several sessions, reporting each one separately.
    ///
    /// The per-session result is the whole point. A broadcast that returns one
    /// success or one error leaves the operator unable to tell which agents got
    /// the prompt, and re-sending to find out delivers it twice to the ones
    /// that already had it.
    ///
    /// Nothing here is best-effort in the other direction either: a session
    /// that cannot take the prompt is named and skipped rather than dropped.
    pub fn broadcast(&self, ids: &[SessionId], bytes: &[u8]) -> Vec<BroadcastResult> {
        ids.iter()
            .map(|id| {
                let refusal = self.broadcast_eligibility(id);
                if let Some(refusal) = refusal {
                    return BroadcastResult {
                        id: id.clone(),
                        refusal: Some(refusal),
                    };
                }
                match self.write(id, bytes) {
                    Ok(()) => BroadcastResult {
                        id: id.clone(),
                        refusal: None,
                    },
                    Err(error) => BroadcastResult {
                        id: id.clone(),
                        refusal: Some(BroadcastRefusal::WriteFailed(error.to_string())),
                    },
                }
            })
            .collect()
    }

    /// Why this session cannot take a broadcast, if it cannot.
    fn broadcast_eligibility(&self, id: &SessionId) -> Option<BroadcastRefusal> {
        let state = lock_state(&self.inner);
        let Some(entry) = state.entries.get(id) else {
            return Some(BroadcastRefusal::Missing);
        };
        match entry.session.status {
            // A permission prompt is a specific question with a small set of
            // valid answers. Typing a paragraph of prompt text at it answers
            // something — just not what the operator meant, and possibly
            // "yes". These are answered one at a time, deliberately.
            SessionStatus::NeedsApproval => Some(BroadcastRefusal::NeedsApproval),
            _ if state.focused.as_ref() == Some(id) && state.operator_edited.contains(id) => {
                Some(BroadcastRefusal::FocusedAndEdited)
            }
            _ if entry.pty.is_none() => Some(BroadcastRefusal::NotRunning),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    
    
    
    use crate::registry::testing::*;
    use std::path::Path;
    
    

    #[test]
    fn a_broadcast_reports_every_session_separately() {
        // One overall status would leave the operator unable to tell which
        // agents got the prompt, and re-sending to find out delivers it twice
        // to the ones that already had it.
        let registry = SessionRegistry::new();
        let running = SessionId::new(1);
        let stopped = SessionId::new(2);
        insert_session(&registry, &running, SessionStatus::Working);
        insert_session(&registry, &stopped, SessionStatus::Exited);
        let missing = SessionId::new(9);

        let results = registry.broadcast(&[running.clone(), stopped.clone(), missing.clone()], b"hi");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, running);
        assert_eq!(results[1].refusal, Some(BroadcastRefusal::NotRunning));
        assert_eq!(results[2].refusal, Some(BroadcastRefusal::Missing));
    }

    #[test]
    fn a_session_waiting_for_a_permission_decision_is_never_broadcast_to() {
        // A permission prompt is a specific question with a small set of valid
        // answers. Typing a paragraph at it answers something — just not what
        // the operator meant, and possibly "yes".
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::NeedsApproval);
        let results = registry.broadcast(&[id], b"go ahead and refactor everything");
        assert_eq!(results[0].refusal, Some(BroadcastRefusal::NeedsApproval));
        assert!(!results[0].delivered());
    }

    #[test]
    fn a_session_that_is_merely_asking_a_question_still_receives_the_prompt() {
        // AwaitingInput is free-text; refusing it too would make broadcast
        // useless for the case it is most obviously for.
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::AwaitingInput);
        // No pty is attached in this fixture, so the refusal must be about the
        // process rather than about the status.
        let results = registry.broadcast(&[id], b"the answer");
        assert_eq!(results[0].refusal, Some(BroadcastRefusal::NotRunning));
    }

    #[test]
    fn focused_partial_input_holds_a_queue_until_the_pane_is_defocused() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::Idle, writes.clone());
        registry.focus(Some(id.clone())).expect("focus");
        registry
            .write_user_input(&id, b"partial")
            .expect("typed input");
        registry.enqueue_prompt(&id, "queued next").expect("enqueue");

        let session = registry.snapshot().into_iter().next().expect("session");
        assert_eq!(session.queue_paused, Some(crate::queue::PauseReason::FocusedAndEdited));
        assert_eq!(session.queued_prompts, 1);
        assert_eq!(writes.lock().expect("writes").len(), 1, "queue fired into the edit");

        registry.focus(None).expect("defocus");
        let writes = writes.lock().expect("writes").clone();
        assert_eq!(writes.len(), 2, "defocus did not release the queue: {writes:?}");
        assert!(writes[1].contains("queued next"), "{writes:?}");
        assert_eq!(registry.snapshot()[0].queue_paused, None);
    }

    #[test]
    fn an_explicit_send_releases_the_focused_edit_guard() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::Idle, writes.clone());
        registry.focus(Some(id.clone())).expect("focus");
        registry.write_user_input(&id, b"partial").expect("typed input");
        registry.enqueue_prompt(&id, "after explicit send").expect("enqueue");

        registry.write_user_input(&id, b"\r").expect("explicit send");
        assert_eq!(registry.snapshot()[0].queue_paused, None);
        apply_test_hook(&registry, HookEvent {
            agent: Agent::Claude,
            session_id: None,
            cwd: Some(Path::new(".").to_path_buf()),
            signal: HookSignal::Stop,
            progress: None,
            approval: None,
        });
        let writes = writes.lock().expect("writes").clone();
        assert!(writes.iter().any(|write| write.contains("after explicit send")), "{writes:?}");
    }

    #[test]
    fn terminal_echo_does_not_clear_the_guard_but_agent_activity_does() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::Idle, writes);
        registry.focus(Some(id.clone())).expect("focus");
        registry.write_user_input(&id, b"partial").expect("typed input");
        registry.enqueue_prompt(&id, "after output").expect("enqueue");

        feed(&registry, &id, b"local terminal echo");
        assert_eq!(registry.snapshot()[0].queue_paused, Some(crate::queue::PauseReason::FocusedAndEdited));

        apply_test_hook(&registry, HookEvent {
            agent: Agent::Claude,
            session_id: None,
            cwd: Some(Path::new(".").to_path_buf()),
            signal: HookSignal::UserPromptSubmit,
            progress: None,
            approval: None,
        });
        assert_eq!(registry.snapshot()[0].queue_paused, None);
    }

    #[test]
    fn a_focused_edit_is_a_distinct_broadcast_refusal() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::Working, writes);
        registry.focus(Some(id.clone())).expect("focus");
        registry.write_user_input(&id, b"partial").expect("typed input");

        let results = registry.broadcast(std::slice::from_ref(&id), b"broadcast");
        assert_eq!(results[0].refusal, Some(BroadcastRefusal::FocusedAndEdited));

        registry.write_user_input(&id, b"\r").expect("explicit send");
        let results = registry.broadcast(&[id], b"broadcast");
        assert!(results[0].delivered(), "{results:?}");
    }

    #[test]
    fn broadcasting_to_nothing_is_an_empty_report_rather_than_an_error() {
        let registry = SessionRegistry::new();
        assert!(registry.broadcast(&[], b"hi").is_empty());
    }

    #[test]
    fn every_refusal_reads_differently_to_the_operator() {
        // They are acted on differently: start it, answer it, or look at why
        // the write failed.
        let refusals = [
            BroadcastRefusal::Missing.to_string(),
            BroadcastRefusal::NotRunning.to_string(),
            BroadcastRefusal::NeedsApproval.to_string(),
            BroadcastRefusal::FocusedAndEdited.to_string(),
            BroadcastRefusal::WriteFailed("pipe closed".into()).to_string(),
        ];
        let unique: std::collections::BTreeSet<_> = refusals.iter().collect();
        assert_eq!(unique.len(), 5, "{refusals:?}");
        assert!(refusals[4].contains("pipe closed"));
    }

    #[test]
    fn a_queued_prompt_is_sent_when_the_session_reports_idle() {
        // End to end through the registry: the prompt leaves the queue, reaches
        // the pty, and is framed the way a typed reply is.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::Working, writes.clone());

        registry.enqueue_prompt(&id, "do the next thing").expect("enqueue");
        assert!(writes.lock().expect("writes").is_empty(), "sent while busy");
        assert_eq!(registry.snapshot()[0].queued_prompts, 1);

        // The same signal the fleet row is drawn from.
        apply_test_hook(&registry, HookEvent {
            agent: Agent::Claude,
            session_id: None,
            cwd: Some(Path::new(".").to_path_buf()),
            signal: HookSignal::Stop,
            progress: None,
            approval: None,
        });

        let sent = writes.lock().expect("writes").clone();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("do the next thing"), "{sent:?}");
        assert!(sent[0].starts_with("\u{1b}[200~"), "not bracketed paste: {sent:?}");
        assert!(sent[0].ends_with("\u{1b}[201~\r"), "no submit: {sent:?}");
        assert_eq!(registry.snapshot()[0].queued_prompts, 0);
    }

    #[test]
    fn a_prompt_queued_against_an_idle_session_fires_at_once() {
        // Otherwise the queue is only usable as a backlog, never as "and then
        // do this".
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::Idle, writes.clone());
        registry.enqueue_prompt(&id, "start now").expect("enqueue");
        assert_eq!(writes.lock().expect("writes").len(), 1);
    }

    #[test]
    fn a_session_waiting_for_permission_does_not_receive_its_queue() {
        // Prompt text at a permission prompt answers something, just not what
        // was queued.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::NeedsApproval, writes.clone());
        registry.enqueue_prompt(&id, "carry on").expect("enqueue");

        assert!(writes.lock().expect("writes").is_empty());
        let session = registry.snapshot().into_iter().next().expect("session");
        assert_eq!(session.queue_paused, Some(crate::queue::PauseReason::NeedsApproval));
        assert_eq!(session.queued_prompts, 1, "the prompt was consumed");
    }

    #[test]
    fn resuming_a_paused_queue_delivers_the_prompt() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        writable_session(&registry, &id, SessionStatus::NeedsApproval, writes.clone());
        registry.enqueue_prompt(&id, "carry on").expect("enqueue");

        // The operator answers, the session goes idle, and they resume.
        {
            let mut state = lock_state(&registry.inner);
            let entry = state.entries.get_mut(&id).expect("entry");
            entry.session.status = SessionStatus::Idle;
        }
        registry.resume_queue(&id).expect("resume");
        let sent = writes.lock().expect("writes").clone();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("carry on"));
    }

    #[test]
    fn a_queue_survives_being_written_to_the_store_and_restored() {
        // Retyping the backlog is the one thing the queue exists to avoid.
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Working);
        registry.enqueue_prompt(&id, "first").expect("enqueue");
        registry.enqueue_prompt(&id, "second").expect("enqueue");

        let restored = SessionRegistry::from_store(registry.store_snapshot());
        let prompts = restored.queued_prompts(&id).expect("prompts");
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].text, "first");
        // A restored session is not running, so its queue must not fire at
        // whatever status the restore left behind.
        let session = restored.snapshot().into_iter().next().expect("session");
        assert_eq!(session.queue_paused, Some(crate::queue::PauseReason::NotRunning));
        assert_eq!(session.queued_prompts, 2);
    }
}
