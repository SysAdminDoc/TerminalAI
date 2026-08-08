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
        // The session handles are cloned out under the lock and read outside it:
        // one job of a dozen processes is a dozen `OpenProcess` calls, and the
        // registry's lock is on the path of every keystroke.
        let targets: Vec<(SessionId, Option<u32>, Arc<dyn crate::domain::AgentSession>)> = {
            let state = lock_state(&self.inner);
            state
                .entries
                .iter()
                .filter_map(|(id, entry)| {
                    entry
                        .pty
                        .as_ref()
                        .map(|pty| (id.clone(), entry.session.pid, Arc::clone(pty)))
                })
                .collect()
        };
        let sampled: Vec<(SessionId, Option<crate::process_tree::JobUsage>)> = targets
            .into_iter()
            .map(|(id, pid, pty)| {
                // The job is the unit the per-session cap is enforced over, so
                // it is the unit that gets measured. A domain that owns no local
                // job falls back to the supervised process and says so by
                // reporting no process count, rather than claiming a figure
                // covers a tree it never looked at.
                let usage = pty.memory_usage().or_else(|| {
                    pid.and_then(crate::process_tree::private_bytes).map(|bytes| {
                        crate::process_tree::JobUsage {
                            private_bytes: bytes,
                            processes: 0,
                        }
                    })
                });
                (id, usage)
            })
            .collect();
        let mut updated = Vec::new();
        {
            let mut state = lock_state(&self.inner);
            for (id, usage) in sampled {
                let Some(entry) = state.entries.get_mut(&id) else {
                    continue;
                };
                // A reading that could not be taken leaves the previous figure
                // in place: an unreadable handle is a momentary condition, and
                // blanking the row would read as the session using nothing.
                let Some(usage) = usage else { continue };
                let bytes = usage.private_bytes;
                // Zero is the fallback's way of saying it did not count, which
                // is not the same as a job holding no processes.
                let processes = (usage.processes > 0).then_some(usage.processes);
                let limited = cap.is_some_and(|cap| bytes >= cap);
                if entry.session.memory_bytes != Some(bytes)
                    || entry.session.memory_processes != processes
                    || entry.session.memory_limited != limited
                {
                    entry.session.memory_bytes = Some(bytes);
                    entry.session.memory_processes = processes;
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
        // Read on the same wakeup as the transcripts, from the same home. A
        // team directory exists only while its team does, and it is a few bytes
        // — a second timer for it would cost more than the read.
        let team_reads: Vec<(SessionId, Option<String>)> = {
            let state = lock_state(&self.inner);
            state
                .entries
                .values()
                .filter(|entry| entry.session.status.is_live())
                .filter(|entry| entry.session.agent == crate::Agent::Claude)
                .filter_map(|entry| {
                    entry
                        .spec
                        .session_id
                        .as_ref()
                        .map(|id| (entry.session.id.clone(), Some(id.clone())))
                })
                .collect()
        };
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

        // Outside the lock: this reads files.
        let teams: Vec<(SessionId, Option<Vec<String>>)> = team_reads
            .into_iter()
            .filter_map(|(id, session_id)| {
                session_id.map(|session_id| (id, crate::teams::teammates(home, &session_id)))
            })
            .collect();

        let mut updated = Vec::new();
        let mut spend_deltas: Vec<(String, f64)> = Vec::new();
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
                        spend_deltas.push((entry.session.id.0.clone(), update.cost_usd - previous));
                        entry.session.cost_usd = Some(update.cost_usd);
                        changed = true;
                    }
                    // The per-session budget, enforced here because here is
                    // where the money is counted. The agent's own flag cannot
                    // do it — `--max-budget-usd` binds under `--print` only —
                    // so a cap that is offered has to be kept by the ledger or
                    // not offered at all.
                    if let Some(budget) = entry.session.budget_usd {
                        let spent = update.cost_usd >= budget;
                        if spent && !entry.session.budget_exhausted {
                            entry.session.budget_exhausted = true;
                            // Paused once, at the crossing. An operator who
                            // decides to carry on can resume; the row goes on
                            // saying the budget is spent either way, so the
                            // override is informed rather than silent.
                            entry.queue.pause(crate::queue::PauseReason::BudgetExhausted);
                            entry.session.queue_paused = entry.queue.paused();
                            tracing::info!(
                                session = %id,
                                budget,
                                cost = update.cost_usd,
                                "session budget spent; its queue is paused and broadcasts skip it"
                            );
                            changed = true;
                        } else if !spent && entry.session.budget_exhausted {
                            // Only reachable if the cap is raised or the ledger
                            // is rebuilt lower. Clearing the flag without
                            // touching the pause leaves the operator's own
                            // decision about the queue where they left it.
                            entry.session.budget_exhausted = false;
                            changed = true;
                        }
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
            // A team that ended takes its directory with it, so this clears
            // as well as sets — a row that stopped being a lead must stop
            // naming the teammates it used to have.
            for (id, names) in teams {
                let Some(entry) = state.entries.get_mut(&id) else {
                    continue;
                };
                if entry.session.teammates != names {
                    entry.session.teammates = names;
                    if !updated.iter().any(|session: &Session| session.id == id) {
                        updated.push(entry.session.clone());
                    }
                }
            }
            if !spend_deltas.is_empty() {
                let now = SystemTime::now();
                for (session, delta) in spend_deltas.drain(..) {
                    state.spend.record_session_at(now, Some(&session), delta);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::registry::testing::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// A scratch home, removed when the test ends. Transcript discovery reads
    /// real directories, so the only honest way to drive this path is to write
    /// one.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-budget-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Scratch(dir)
    }

    /// One priced assistant record, in the shape the JSONL actually carries.
    fn write_claude_turn(home: &Path, cwd: &Path, request: &str, output_tokens: u64) {
        let file = home
            .join(".claude")
            .join("projects")
            .join(crate::tail::claude_project_slug(cwd))
            .join("11111111-2222-3333-4444-555555555555.jsonl");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
        let mut handle = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .expect("open");
        writeln!(
            handle,
            r#"{{"type":"assistant","sessionId":"11111111-2222-3333-4444-555555555555","requestId":"{request}","message":{{"role":"assistant","model":"claude-opus-4-20250514","content":[{{"type":"text","text":"done"}}],"usage":{{"input_tokens":1000,"output_tokens":{output_tokens}}}}}}}"#
        )
        .expect("write");
        handle.flush().expect("flush");
    }

    /// A live row whose transcript will be found under `home`.
    fn budgeted_row(registry: &SessionRegistry, id: &SessionId, cwd: &Path, budget: Option<f64>) {
        live_entry(registry, id.clone(), Agent::Claude, None);
        let mut state = lock_state(&registry.inner);
        let entry = state.entries.get_mut(id).expect("row");
        entry.session.cwd = cwd.to_path_buf();
        entry.session.budget_usd = budget;
        entry.spec.max_budget_usd = budget;
    }

    #[test]
    fn a_session_that_spends_its_budget_stops_being_given_work() {
        // The enforcement the launcher promises. It cannot be the agent's own
        // `--max-budget-usd`: that flag binds under `--print`, and nothing
        // supervised here is in print mode. So the ledger has to keep it, and
        // this is the test that says it does.
        let home = scratch("spent");
        let cwd = Path::new("/repos/shop");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        budgeted_row(&registry, &id, cwd, Some(0.01));
        registry.enqueue_prompt(&id, "next task").expect("queued");

        write_claude_turn(&home.0, cwd, "req-1", 500);
        assert_eq!(registry.poll_transcripts(&home.0), 1, "the row changed");

        let session = &registry.snapshot()[0];
        assert!(
            session.cost_usd.expect("priced") >= 0.01,
            "the fixture has to actually exceed the cap: {:?}",
            session.cost_usd
        );
        assert!(session.budget_exhausted, "the crossing is recorded");
        assert_eq!(
            session.queue_paused,
            Some(crate::queue::PauseReason::BudgetExhausted),
            "queued work stops going out"
        );
        assert_eq!(
            registry.broadcast(std::slice::from_ref(&id), b"everyone carry on")[0].refusal,
            Some(BroadcastRefusal::BudgetExhausted),
            "and a fleet-wide prompt does not route around it"
        );
        assert_eq!(registry.admission_snapshot().budget_exhausted_sessions, 1);
    }

    #[test]
    fn a_session_under_its_budget_is_left_alone_and_one_with_no_budget_is_never_stopped() {
        // Both halves matter. A cap that trips early is a fleet that stops
        // working for no stated reason, and a session launched with no cap must
        // never acquire one from this code path.
        let home = scratch("under");
        let cwd = Path::new("/repos/shop");
        let registry = SessionRegistry::new();
        let capped = SessionId::new(1);
        let uncapped = SessionId::new(2);
        budgeted_row(&registry, &capped, cwd, Some(1_000.0));
        budgeted_row(&registry, &uncapped, cwd, None);
        registry.enqueue_prompt(&capped, "next task").expect("queued");

        write_claude_turn(&home.0, cwd, "req-1", 500);
        registry.poll_transcripts(&home.0);

        for session in registry.snapshot() {
            assert!(
                session.cost_usd.is_some_and(|cost| cost > 0.0),
                "both rows read the same transcript"
            );
            assert!(!session.budget_exhausted, "{:?} was stopped", session.id);
            assert_eq!(session.queue_paused, None);
        }
        assert_eq!(registry.admission_snapshot().budget_exhausted_sessions, 0);
    }

    #[test]
    fn a_lead_names_its_teammates_and_a_row_with_no_team_names_nobody() {
        // The density argument only holds if a row's cost is legible, and since
        // agent teams a row can be a lead plus several separate agent
        // instances shown as one line.
        let home = scratch("teams");
        let cwd = Path::new("/repos/shop");
        let registry = SessionRegistry::new();
        let lead = SessionId::new(1);
        let solo = SessionId::new(2);
        budgeted_row(&registry, &lead, cwd, None);
        budgeted_row(&registry, &solo, cwd, None);
        let native = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        {
            let mut state = lock_state(&registry.inner);
            state.entries.get_mut(&lead).expect("row").spec.session_id = Some(native.to_owned());
            state.entries.get_mut(&solo).expect("row").spec.session_id =
                Some("11111111-2222-4333-8444-555555555555".to_owned());
        }
        let config = crate::teams::team_config_path(&home.0, native).expect("path");
        std::fs::create_dir_all(config.parent().expect("parent")).expect("dirs");
        std::fs::write(&config, r#"{"members":[{"name":"reviewer"},{"name":"tester"}]}"#)
            .expect("team file");

        registry.poll_transcripts(&home.0);
        let sessions = registry.snapshot();
        let lead_row = sessions.iter().find(|s| s.id == lead).expect("lead");
        let solo_row = sessions.iter().find(|s| s.id == solo).expect("solo");
        assert_eq!(
            lead_row.teammates,
            Some(vec!["reviewer".to_owned(), "tester".to_owned()])
        );
        assert_eq!(
            solo_row.teammates, None,
            "a session with no team must report nothing, not zero"
        );

        // A team directory goes when the team does, and the row has to follow.
        std::fs::remove_dir_all(config.parent().expect("parent")).expect("team ends");
        registry.poll_transcripts(&home.0);
        assert_eq!(
            registry
                .snapshot()
                .iter()
                .find(|s| s.id == lead)
                .expect("lead")
                .teammates,
            None,
            "the row still names teammates from a team that has ended"
        );
    }

    #[test]
    fn a_row_with_no_assigned_session_id_is_never_attributed_a_team() {
        // The id is the only thing tying a row to a team directory. Without one
        // there is no honest way to look, and guessing would attribute somebody
        // else's team to this row.
        let home = scratch("no-id");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        budgeted_row(&registry, &id, Path::new("/repos/shop"), None);
        {
            let mut state = lock_state(&registry.inner);
            state.entries.get_mut(&id).expect("row").spec.session_id = None;
        }
        registry.poll_transcripts(&home.0);
        assert_eq!(registry.snapshot()[0].teammates, None);
    }

    #[test]
    fn every_agent_is_named_as_budget_enforced_because_the_ledger_reads_every_transcript() {
        // This used to name Claude alone, on the strength of a flag that only
        // works under `--print`. The claim the header makes and the enforcement
        // that exists have to be the same claim.
        let registry = SessionRegistry::new();
        let enforced = registry.admission_snapshot().budget_enforced_agents;
        for agent in Agent::ALL {
            assert!(
                enforced.iter().any(|name| name == agent.command_name()),
                "{agent:?} missing from {enforced:?}"
            );
        }
        assert_eq!(enforced.len(), Agent::ALL.len());
    }
}
