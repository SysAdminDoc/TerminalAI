//! What the fleet measures about a session it is already running: private
//! commit, and the cost and token counts the agent writes to its own transcript.
//!
//! Split out of `mod.rs` for size. Both run on one timer rather than two: a
//! wakeup that already exists is cheaper than a second one, and neither figure
//! moves fast enough to need a tighter loop.

use super::*;

impl SessionRegistry {
    /// Sample each live session's private commit.
    ///
    /// Runs on the same cadence as transcript polling rather than on its own
    /// timer: one wakeup that already exists is cheaper than a second one, and
    /// memory does not move fast enough to need a tighter loop.
    pub fn sample_memory(&self) -> usize {
        let cap = {
            let state = lock_state(&self.inner);
            state.admission.session_memory_cap_bytes
        };
        let sampled: Vec<(SessionId, Option<u64>)> = {
            let state = lock_state(&self.inner);
            state
                .entries
                .iter()
                .filter(|(_, entry)| entry.pty.is_some())
                .filter_map(|(id, entry)| {
                    entry
                        .session
                        .pid
                        .map(|pid| (id.clone(), crate::process_tree::private_bytes(pid)))
                })
                .collect()
        };
        let mut updated = Vec::new();
        {
            let mut state = lock_state(&self.inner);
            for (id, bytes) in sampled {
                let Some(entry) = state.entries.get_mut(&id) else {
                    continue;
                };
                // A reading that could not be taken leaves the previous figure
                // in place: an unreadable handle is a momentary condition, and
                // blanking the row would read as the session using nothing.
                let Some(bytes) = bytes else { continue };
                let limited = cap.is_some_and(|cap| bytes >= cap);
                if entry.session.memory_bytes != Some(bytes)
                    || entry.session.memory_limited != limited
                {
                    entry.session.memory_bytes = Some(bytes);
                    entry.session.memory_limited = limited;
                    updated.push(entry.session.clone());
                }
            }
        }
        let count = updated.len();
        for session in updated {
            self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        }
        count
    }

    /// Read whatever each live session's transcript has appended, and fold the
    /// result into its row.
    ///
    /// Called on a timer rather than driven by a filesystem watcher: both CLIs
    /// append continuously during a turn, so a watcher would fire hundreds of
    /// times per response for the same three fields. Each read is incremental —
    /// only the bytes since the last poll — so the cost is proportional to what
    /// the agent wrote, not to how large the transcript has grown.
    ///
    /// Returns how many rows changed.
    pub fn poll_transcripts(&self, home: &std::path::Path) -> usize {
        // Snapshot the work under the lock, then read files without holding it:
        // a slow disk must not stall status ingestion.
        let targets: Vec<(
            SessionId,
            crate::Agent,
            std::path::PathBuf,
            SystemTime,
            Option<String>,
        )> = {
            let state = lock_state(&self.inner);
            state
                .entries
                .values()
                .filter(|entry| entry.session.status.is_live())
                .map(|entry| {
                    (
                        entry.session.id.clone(),
                        entry.session.agent,
                        entry.session.cwd.clone(),
                        entry.session.started_at,
                        entry.spec.session_id.clone(),
                    )
                })
                .collect()
        };
        if targets.is_empty() {
            return 0;
        }

        // Keep the expensive transcript work behind its own lock. The state
        // lock is reacquired only after every file read and directory walk has
        // completed, so output and hook ingestion remain responsive.
        let updates = {
            let mut tails = self
                .inner
                .tails
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            targets
                .into_iter()
                .map(|(id, agent, cwd, started_at, session_id)| {
                    let update = tails.poll(
                        &id.0,
                        agent,
                        home,
                        &cwd,
                        started_at,
                        session_id.as_deref(),
                    );
                    (id, update)
                })
                .collect::<Vec<_>>()
        };

        let mut updated = Vec::new();
        let mut spend_deltas: Vec<f64> = Vec::new();
        {
            let mut state = lock_state(&self.inner);
            for (id, update) in updates {
                if !update.changed {
                    continue;
                }
                let Some(entry) = state.entries.get_mut(&id) else {
                    continue;
                };
                let mut changed = false;
                // The agent's own id is what `--resume` takes. Never overwrite
                // one the hooks already reported with something read later.
                if entry.session.resume_id.is_none() {
                    if let Some(native) = update
                        .native_session_id
                        .filter(|native| is_valid_resume_id(native))
                    {
                        entry.session.resume_id = Some(native);
                        changed = true;
                    }
                }
                if let Some(message) = update.last_message {
                    if entry.session.last_message.as_deref() != Some(message.as_str()) {
                        entry.session.last_message = Some(message);
                        // The transcript grew, so the agent is producing work
                        // even if its status has not moved.
                        entry.session.note_progress_at(SystemTime::now());
                        changed = true;
                    }
                }
                // Zero requests means nothing was read, which is not the same as
                // a session that cost nothing — leave the row unpriced so the
                // header keeps saying the spend is unknown.
                if update.totals.requests > 0 {
                    if entry.session.cost_usd != Some(update.cost_usd) {
                        // A session reports a running total; the ledger wants
                        // the increase, so the fleet window counts the money
                        // once and counts it when it was spent.
                        let previous = entry.session.cost_usd.unwrap_or(0.0);
                        spend_deltas.push(update.cost_usd - previous);
                        entry.session.cost_usd = Some(update.cost_usd);
                        changed = true;
                    }
                    if entry.session.tokens != Some(update.totals) {
                        entry.session.tokens = Some(update.totals);
                        changed = true;
                    }
                }
                // Deliberately outside the `requests > 0` guard above and never
                // overwriting a reading the agent stated itself: Codex reports
                // its own window over the app-server transport, and a derived
                // reading has no window at all, so letting the transcript win
                // would replace a percentage with a bare number.
                if let Some(used) = update.context_tokens {
                    let derived = crate::context::ContextUsage::derived(used);
                    let agent_reported = matches!(
                        entry.session.context,
                        Some(existing) if existing.source == crate::context::ContextSource::Agent
                    );
                    if !agent_reported && entry.session.context != Some(derived) {
                        entry.session.context = Some(derived);
                        changed = true;
                    }
                }
                if changed {
                    updated.push(entry.session.clone());
                }
            }
            if !spend_deltas.is_empty() {
                let now = SystemTime::now();
                for delta in spend_deltas.drain(..) {
                    state.spend.record_at(now, delta);
                }
                let window = state.admission.spend_window;
                state.spend.prune_at(now, window);
            }
        }

        let count = updated.len();
        for session in updated {
            self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        }
        count
    }

    /// Drop a finished session's transcript reader.
    pub fn forget_transcript(&self, id: &SessionId) {
        self.inner
            .tails
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .forget(&id.0);
    }
}
