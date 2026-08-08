//! Prompts waiting their turn on one session.
//!
//! An agent takes minutes per turn, so the operator's next three instructions
//! are known long before the current one finishes. Queueing them turns a
//! session from something you must come back to into something you can load up
//! and leave.
//!
//! Everything here exists to make that safe:
//!
//! - **Completion is a reported status, never a timer.** The queue advances
//!   when the session enters `Idle` through the same signal the fleet row uses.
//!   A timer would send the next prompt into the middle of a long tool call,
//!   where it is either ignored or — worse — read as an answer to whatever the
//!   agent asked in between.
//! - **An attention state pauses rather than answers.** If the run ends waiting
//!   for a permission decision or a question, sending the next queued prompt
//!   would be answering blind: prompt text at a yes/no prompt means something,
//!   just not what was queued. The queue stops and says why.
//! - **One prompt is dispatched at a time.** Writing to the pty does not change
//!   the session's status; the agent does, a moment later. Without a hold
//!   between "sent" and "the session picked it up", the next `Idle` observation
//!   would fire the whole queue in one burst.
//!
//! The queue is per session and lives with it: a restart of the daemon that
//! restores a session restores what it was queued to do next.

use std::collections::VecDeque;

use crate::session::SessionStatus;

/// Most prompts one session will hold. A queue longer than this is a script,
/// and a script wants the work queue rather than one session's backlog.
pub const MAX_QUEUED: usize = 32;

/// Longest single queued prompt. Matches the control plane's write limit, so a
/// prompt that can be queued can always be delivered.
pub const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// Why a queue stopped advancing on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// The run ended waiting for a permission decision.
    NeedsApproval,
    /// The run ended asking a question.
    AwaitingInput,
    /// The session is no longer running.
    NotRunning,
    /// The focused terminal has unsubmitted operator input.
    FocusedAndEdited,
    /// The operator paused it.
    Operator,
    /// The session's ledger cost reached the per-session budget it was launched
    /// with. Applied once, at the crossing, so an operator who decides to carry
    /// on can resume — the row keeps saying the budget is spent either way.
    BudgetExhausted,
}

impl PauseReason {
    /// True when the operator has to deal with the session before the queue can
    /// sensibly continue.
    pub fn needs_operator(self) -> bool {
        matches!(
            self,
            Self::NeedsApproval | Self::AwaitingInput | Self::BudgetExhausted
        )
    }
}

/// One prompt waiting its turn.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueuedPrompt {
    /// Stable across reorders and edits, so the UI can address one entry
    /// without depending on its position — which the operator is changing.
    pub id: u64,
    pub text: String,
}

/// What the caller should do after a queue observed something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAction {
    /// Nothing to do.
    Idle,
    /// Write this prompt to the session.
    Send(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromptQueue {
    entries: VecDeque<QueuedPrompt>,
    #[serde(default)]
    paused: Option<PauseReason>,
    /// A prompt has been written but the session has not yet left `Idle`.
    ///
    /// Not serialized: after a restart nothing is in flight, and a stale hold
    /// would stop the queue forever with no way for the operator to see why.
    #[serde(skip)]
    awaiting_pickup: bool,
    #[serde(default)]
    next_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    #[error("a queued prompt cannot be empty")]
    Empty,
    #[error("a queued prompt cannot exceed {MAX_PROMPT_BYTES} bytes")]
    TooLong,
    #[error("this session already has the maximum of {MAX_QUEUED} queued prompts")]
    Full,
    #[error("no queued prompt with id {0}")]
    Missing(u64),
}

impl PromptQueue {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &VecDeque<QueuedPrompt> {
        &self.entries
    }

    pub fn paused(&self) -> Option<PauseReason> {
        self.paused
    }

    /// Add a prompt to the back of the queue.
    pub fn push(&mut self, text: &str) -> Result<u64, QueueError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(QueueError::Empty);
        }
        if text.len() > MAX_PROMPT_BYTES {
            return Err(QueueError::TooLong);
        }
        if self.entries.len() >= MAX_QUEUED {
            return Err(QueueError::Full);
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.entries.push_back(QueuedPrompt {
            id,
            text: text.to_owned(),
        });
        Ok(id)
    }

    /// Replace the text of a prompt that has not fired yet.
    pub fn edit(&mut self, id: u64, text: &str) -> Result<(), QueueError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(QueueError::Empty);
        }
        if text.len() > MAX_PROMPT_BYTES {
            return Err(QueueError::TooLong);
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(QueueError::Missing(id))?;
        entry.text = text.to_owned();
        Ok(())
    }

    /// Withdraw a prompt before it fires.
    pub fn remove(&mut self, id: u64) -> Result<(), QueueError> {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        if self.entries.len() == before {
            return Err(QueueError::Missing(id));
        }
        Ok(())
    }

    /// Move a prompt to a new position.
    ///
    /// Addressed by id rather than by index because the operator is changing
    /// the indices, and a reorder that raced a fired prompt would otherwise
    /// move the wrong entry.
    pub fn reorder(&mut self, id: u64, to: usize) -> Result<(), QueueError> {
        let from = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(QueueError::Missing(id))?;
        let entry = self.entries.remove(from).expect("position just found");
        let to = to.min(self.entries.len());
        self.entries.insert(to, entry);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Stop advancing until the operator resumes.
    pub fn pause(&mut self, reason: PauseReason) {
        self.paused = Some(reason);
    }

    /// Hold automatic delivery while the operator is composing in the focused
    /// pane. This is a transient guard, so it never overrides a stronger
    /// attention or operator pause already visible to the user.
    pub fn hold_for_focus_edit(&mut self) {
        if self.paused.is_none() {
            self.paused = Some(PauseReason::FocusedAndEdited);
        }
    }

    /// Release only the transient focused-input hold. Other pause reasons are
    /// independent and must remain in force.
    pub fn clear_focus_edit(&mut self) {
        if self.paused == Some(PauseReason::FocusedAndEdited) {
            self.paused = None;
        }
    }

    /// Resume after the operator has dealt with whatever paused it.
    ///
    /// Clears the in-flight hold as well: if the queue paused while a prompt
    /// was believed to be in flight, keeping the hold would make resuming do
    /// nothing at all.
    pub fn resume(&mut self) {
        self.paused = None;
        self.awaiting_pickup = false;
    }

    /// Observe a session's status and decide what the queue should do.
    ///
    /// Called on every status change, which is the same signal the fleet row is
    /// drawn from — deliberately, so the queue can never advance on evidence
    /// the operator cannot also see.
    pub fn observe(&mut self, status: SessionStatus) -> QueueAction {
        // An attention state pauses. Prompt text is not an answer to a
        // permission prompt, and it is the wrong answer to a question.
        match status {
            SessionStatus::NeedsApproval => {
                self.paused = Some(PauseReason::NeedsApproval);
                return QueueAction::Idle;
            }
            SessionStatus::AwaitingInput => {
                self.paused = Some(PauseReason::AwaitingInput);
                return QueueAction::Idle;
            }
            SessionStatus::Exited => {
                self.paused = Some(PauseReason::NotRunning);
                return QueueAction::Idle;
            }
            _ => {}
        }

        if status != SessionStatus::Idle {
            // The session moved on, so whatever was sent has been picked up.
            self.awaiting_pickup = false;
            return QueueAction::Idle;
        }

        if self.paused.is_some() || self.awaiting_pickup || self.entries.is_empty() {
            return QueueAction::Idle;
        }
        let next = self.entries.pop_front().expect("checked non-empty");
        // Held until the session leaves Idle. A pty write does not change the
        // status; the agent does, a moment later — without this the next Idle
        // observation would fire the rest of the queue in one burst.
        self.awaiting_pickup = true;
        QueueAction::Send(next.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(prompts: &[&str]) -> PromptQueue {
        let mut queue = PromptQueue::default();
        for prompt in prompts {
            queue.push(prompt).expect("push");
        }
        queue
    }

    #[test]
    fn a_prompt_fires_when_the_session_reports_it_is_idle() {
        let mut queue = queue(&["first", "second"]);
        assert_eq!(queue.observe(SessionStatus::Working), QueueAction::Idle);
        assert_eq!(
            queue.observe(SessionStatus::Idle),
            QueueAction::Send("first".into())
        );
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn the_whole_queue_does_not_fire_in_one_burst() {
        // Writing to the pty does not change the status; the agent does, a
        // moment later. Without the hold, every Idle observation in that window
        // would send another prompt.
        let mut queue = queue(&["first", "second", "third"]);
        assert_eq!(
            queue.observe(SessionStatus::Idle),
            QueueAction::Send("first".into())
        );
        assert_eq!(queue.observe(SessionStatus::Idle), QueueAction::Idle);
        assert_eq!(queue.observe(SessionStatus::Idle), QueueAction::Idle);
        assert_eq!(queue.len(), 2, "more than one prompt left the queue");

        // Once the agent actually picks it up, the next turn proceeds.
        queue.observe(SessionStatus::Working);
        assert_eq!(
            queue.observe(SessionStatus::Idle),
            QueueAction::Send("second".into())
        );
    }

    #[test]
    fn a_run_that_ends_asking_for_permission_pauses_rather_than_answering() {
        // Prompt text at a yes/no prompt answers something — just not what was
        // queued, and possibly "yes".
        let mut queue = queue(&["do the risky thing"]);
        assert_eq!(queue.observe(SessionStatus::NeedsApproval), QueueAction::Idle);
        assert_eq!(queue.paused(), Some(PauseReason::NeedsApproval));
        // And it stays paused even once the session reports Idle again.
        assert_eq!(queue.observe(SessionStatus::Idle), QueueAction::Idle);
        assert_eq!(queue.len(), 1, "the prompt was consumed while paused");
    }

    #[test]
    fn a_run_that_ends_asking_a_question_pauses_too() {
        let mut queue = queue(&["next"]);
        queue.observe(SessionStatus::AwaitingInput);
        assert_eq!(queue.paused(), Some(PauseReason::AwaitingInput));
        assert!(queue.paused().expect("paused").needs_operator());
    }

    #[test]
    fn resuming_after_an_attention_state_sends_the_next_prompt() {
        let mut queue = queue(&["next"]);
        queue.observe(SessionStatus::NeedsApproval);
        queue.resume();
        assert_eq!(queue.paused(), None);
        assert_eq!(
            queue.observe(SessionStatus::Idle),
            QueueAction::Send("next".into())
        );
    }

    #[test]
    fn resuming_clears_an_in_flight_hold_as_well() {
        // If the queue paused while a prompt was believed in flight, keeping
        // the hold would make resume do nothing and look broken.
        let mut queue = queue(&["a", "b"]);
        queue.observe(SessionStatus::Idle);
        queue.pause(PauseReason::Operator);
        queue.resume();
        assert_eq!(queue.observe(SessionStatus::Idle), QueueAction::Send("b".into()));
    }

    #[test]
    fn focused_editing_holds_a_prompt_until_the_guard_is_cleared() {
        let mut queue = queue(&["next"]);
        queue.hold_for_focus_edit();
        assert_eq!(queue.paused(), Some(PauseReason::FocusedAndEdited));
        assert_eq!(queue.observe(SessionStatus::Idle), QueueAction::Idle);
        assert_eq!(queue.len(), 1);

        queue.clear_focus_edit();
        assert_eq!(queue.paused(), None);
        assert_eq!(
            queue.observe(SessionStatus::Idle),
            QueueAction::Send("next".into())
        );
    }

    #[test]
    fn focused_editing_does_not_override_a_stronger_pause() {
        let mut queue = queue(&["next"]);
        queue.pause(PauseReason::Operator);
        queue.hold_for_focus_edit();
        assert_eq!(queue.paused(), Some(PauseReason::Operator));
        queue.clear_focus_edit();
        assert_eq!(queue.paused(), Some(PauseReason::Operator));
    }

    #[test]
    fn an_exited_session_pauses_its_queue_rather_than_losing_it() {
        // The prompts are still what the operator wanted done; reviving the
        // session should not mean retyping them.
        let mut queue = queue(&["one", "two"]);
        queue.observe(SessionStatus::Exited);
        assert_eq!(queue.paused(), Some(PauseReason::NotRunning));
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn a_queued_prompt_can_be_edited_and_withdrawn_before_it_fires() {
        let mut queue = PromptQueue::default();
        let first = queue.push("typo").expect("push");
        let second = queue.push("withdraw me").expect("push");
        queue.edit(first, "fixed").expect("edit");
        queue.remove(second).expect("remove");
        assert_eq!(
            queue.observe(SessionStatus::Idle),
            QueueAction::Send("fixed".into())
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn entries_are_addressed_by_id_because_the_operator_is_moving_them() {
        let mut queue = queue(&["a", "b", "c"]);
        let ids: Vec<u64> = queue.entries().iter().map(|entry| entry.id).collect();
        queue.reorder(ids[2], 0).expect("reorder");
        let order: Vec<&str> = queue
            .entries()
            .iter()
            .map(|entry| entry.text.as_str())
            .collect();
        assert_eq!(order, vec!["c", "a", "b"]);
        // Ids survive the move, so a later edit still finds the right entry.
        queue.edit(ids[0], "edited a").expect("edit");
        assert_eq!(queue.entries()[1].text, "edited a");
    }

    #[test]
    fn reordering_past_the_end_lands_at_the_end_rather_than_failing() {
        let mut queue = queue(&["a", "b"]);
        let first = queue.entries()[0].id;
        queue.reorder(first, 99).expect("reorder");
        assert_eq!(queue.entries()[1].text, "a");
    }

    #[test]
    fn editing_or_removing_a_prompt_that_already_fired_says_so() {
        // The operator's click raced the queue. Reporting it is what lets the
        // UI tell them the prompt is already running.
        let mut queue = queue(&["a"]);
        let id = queue.entries()[0].id;
        queue.observe(SessionStatus::Idle);
        assert_eq!(queue.edit(id, "too late"), Err(QueueError::Missing(id)));
        assert_eq!(queue.remove(id), Err(QueueError::Missing(id)));
    }

    #[test]
    fn an_empty_or_oversized_prompt_is_refused() {
        let mut queue = PromptQueue::default();
        assert_eq!(queue.push("   "), Err(QueueError::Empty));
        assert_eq!(
            queue.push(&"x".repeat(MAX_PROMPT_BYTES + 1)),
            Err(QueueError::TooLong)
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut queue = PromptQueue::default();
        for index in 0..MAX_QUEUED {
            queue.push(&format!("prompt {index}")).expect("push");
        }
        assert_eq!(queue.push("one too many"), Err(QueueError::Full));
    }

    #[test]
    fn ids_are_never_reused_after_a_prompt_is_removed() {
        // A reused id would let a stale edit land on a different prompt.
        let mut queue = PromptQueue::default();
        let first = queue.push("a").expect("push");
        queue.remove(first).expect("remove");
        let second = queue.push("b").expect("push");
        assert_ne!(first, second);
    }

    #[test]
    fn an_in_flight_hold_does_not_survive_a_restart() {
        // Nothing is in flight after a restart, and a stale hold would stop the
        // queue forever with nothing on screen to explain it.
        let mut queue = queue(&["a", "b"]);
        queue.observe(SessionStatus::Idle);
        let json = serde_json::to_string(&queue).expect("encode");
        let mut restored: PromptQueue = serde_json::from_str(&json).expect("decode");
        assert_eq!(
            restored.observe(SessionStatus::Idle),
            QueueAction::Send("b".into())
        );
    }

    #[test]
    fn a_queue_survives_a_restart_with_its_prompts_and_its_pause() {
        let mut queue = queue(&["a", "b"]);
        queue.observe(SessionStatus::NeedsApproval);
        let json = serde_json::to_string(&queue).expect("encode");
        let restored: PromptQueue = serde_json::from_str(&json).expect("decode");
        assert_eq!(restored.len(), 2);
        assert_eq!(restored.paused(), Some(PauseReason::NeedsApproval));
    }
}
