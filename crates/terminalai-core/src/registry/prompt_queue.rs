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
