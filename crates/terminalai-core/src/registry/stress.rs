//! Deterministic fleet-scale exercise for CI and release verification.
//!
//! The harness uses an injected domain rather than real agents. That keeps the
//! profile repeatable, avoids credentials and windows, and still drives the
//! same launch, monitor, hook, scrollback, subscriber and store paths that a
//! live daemon uses.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::agent::{Agent, AgentBinary, Origin};
use crate::domain::{AgentDomain, AgentSession, DomainError, OutputHandler};
use crate::hooks::{HookEvent, HookSignal};
use crate::launch::{spec_for, ResolvedCommand};
use crate::pty::{PtySize, StopOutcome};
use crate::store::SessionStoreSnapshot;
use crate::{AdmissionConfig, SessionRegistry};

use super::{handle_output, lock_state, SUBSCRIBER_QUEUE_CAPACITY};

/// The profile used by the release verification script.
pub const DEFAULT_SESSIONS: usize = 30;
pub const DEFAULT_EVENTS_PER_SESSION: usize = 64;
pub const MAX_SESSIONS: usize = 100;
pub const MAX_EVENTS_PER_SESSION: usize = 2_000;
/// A synthetic profile should exercise the path, not fail because a busy CI
/// host spent a little longer starting threads.
pub const STARTUP_BUDGET: Duration = Duration::from_secs(15);
pub const HOOK_P95_BUDGET: Duration = Duration::from_millis(100);
pub const SNAPSHOT_P95_BUDGET: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LatencySummary {
    pub samples: usize,
    pub min_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecoverySummary {
    pub store_round_trip: bool,
    pub corrupt_store_rejected: bool,
    pub restored_sessions: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StressGates {
    pub startup_under_budget: bool,
    pub hooks_under_budget: bool,
    pub snapshots_under_budget: bool,
    pub scrollback_bounded: bool,
    pub event_queue_bounded: bool,
    pub recovery_proven: bool,
    pub all_pass: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FleetStressReport {
    pub sessions: usize,
    pub events: usize,
    pub startup_ms: f64,
    pub hooks: LatencySummary,
    pub snapshots: LatencySummary,
    pub max_scrollback_bytes: usize,
    pub store_bytes: usize,
    pub event_queue_depth: usize,
    pub dropped_events: u64,
    pub recovery: RecoverySummary,
    pub gates: StressGates,
}

#[derive(Debug)]
struct SyntheticSession {
    pid: u32,
    stopped: AtomicBool,
    wake: Condvar,
    state: Mutex<bool>,
}

impl SyntheticSession {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            stopped: AtomicBool::new(false),
            wake: Condvar::new(),
            state: Mutex::new(false),
        }
    }

    fn stop_now(&self) {
        if !self.stopped.swap(true, Ordering::Release) {
            *self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            self.wake.notify_all();
        }
    }
}

impl AgentSession for SyntheticSession {
    fn write(&self, _bytes: &[u8]) -> Result<(), DomainError> {
        Ok(())
    }

    fn resize(&self, _size: PtySize) -> Result<(), DomainError> {
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn try_wait(&self) -> Result<Option<u32>, DomainError> {
        Ok(self.stopped.load(Ordering::Acquire).then_some(0))
    }

    fn wait_for_exit(&self) -> Result<u32, DomainError> {
        let mut stopped = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*stopped {
            stopped = self
                .wake
                .wait(stopped)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        Ok(0)
    }

    fn kill(&self) -> Result<(), DomainError> {
        self.stop_now();
        Ok(())
    }

    fn stop(&self) -> Result<StopOutcome, DomainError> {
        self.stop_now();
        Ok(StopOutcome::Terminated)
    }
}

#[derive(Debug, Default)]
struct SyntheticDomain {
    next_pid: AtomicU32,
}

impl AgentDomain for SyntheticDomain {
    fn spawn(
        &self,
        _command: &ResolvedCommand,
        _size: PtySize,
        _environment: &[(String, String)],
        _limits: crate::process_tree::JobLimits,
        _on_output: OutputHandler,
    ) -> Result<Arc<dyn AgentSession>, DomainError> {
        let pid = self.next_pid.fetch_add(1, Ordering::Relaxed).saturating_add(10_000);
        Ok(Arc::new(SyntheticSession::new(pid)))
    }
}

/// Run the deterministic profile and return its measurements and gate verdict.
pub fn run(sessions: usize, events_per_session: usize) -> Result<FleetStressReport, String> {
    if !(1..=MAX_SESSIONS).contains(&sessions) {
        return Err(format!("sessions must be between 1 and {MAX_SESSIONS}"));
    }
    if !(1..=MAX_EVENTS_PER_SESSION).contains(&events_per_session) {
        return Err(format!(
            "events per session must be between 1 and {MAX_EVENTS_PER_SESSION}"
        ));
    }

    let domain = Arc::new(SyntheticDomain::default());
    let registry = SessionRegistry::with_domain_and_admission(
        domain,
        AdmissionConfig::new(sessions, None),
    );
    let events = registry.subscribe();
    let cwd = std::env::current_dir().map_err(|error| format!("read stress cwd: {error}"))?;
    let binary = AgentBinary {
        agent: Agent::Claude,
        path: "claude.exe".into(),
        origin: Origin::Configured,
    };

    let startup_started = Instant::now();
    let mut ids = Vec::with_capacity(sessions);
    for index in 0..sessions {
        let mut spec = spec_for(Agent::Claude, &cwd);
        spec.name = Some(format!("synthetic-{index:03}"));
        ids.push(
            registry
                .launch(spec, binary.clone())
                .map_err(|error| format!("launch synthetic session {index}: {error}"))?,
        );
    }
    let startup_deadline = startup_started + STARTUP_BUDGET;
    loop {
        let started = registry
            .snapshot()
            .iter()
            .filter(|session| session.pid.is_some())
            .count();
        if started == sessions {
            break;
        }
        if Instant::now() >= startup_deadline {
            registry.shutdown();
            return Err(format!("only {started}/{sessions} synthetic sessions started"));
        }
        std::thread::yield_now();
    }
    let startup_ms = elapsed_ms(startup_started.elapsed());

    // One oversized chunk proves every row's ring keeps its hard cap; the
    // smaller chunks make the serialized store representative without making
    // this profile spend its whole budget copying 15 MB of identical data.
    for (index, id) in ids.iter().enumerate() {
        let bytes = if index == 0 {
            vec![b'x'; crate::registry::MAX_SCROLLBACK_BYTES + 4096]
        } else {
            vec![b'x'; 64 * 1024]
        };
        handle_output(&registry.inner, id, 1, &bytes);
    }

    let tokens = {
        let state = lock_state(&registry.inner);
        ids.iter()
            .map(|id| {
                state
                    .entries
                    .get(id)
                    .map(|entry| entry.session.hook_token.clone())
                    .ok_or_else(|| format!("synthetic session {id} disappeared"))
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut hook_latencies = Vec::with_capacity(sessions * events_per_session);
    for (index, (id, token)) in ids.iter().zip(tokens.iter()).enumerate() {
        for event_index in 0..events_per_session {
            let signal = if (index + event_index) % 2 == 0 {
                HookSignal::PreToolUse
            } else {
                HookSignal::PostToolUse
            };
            let started = Instant::now();
            let matched = registry.apply_hook_with_token(
                HookEvent {
                    agent: Agent::Claude,
                    session_id: None,
                    cwd: Some(cwd.clone()),
                    signal,
                    progress: None,
                    approval: None,
                },
                Some(token),
            );
            hook_latencies.push(started.elapsed());
            if !matched {
                registry.shutdown();
                return Err(format!("synthetic hook did not match session {id}"));
            }
        }
    }

    let mut snapshot_latencies = Vec::with_capacity(16);
    for _ in 0..16 {
        let started = Instant::now();
        let snapshot = registry.snapshot();
        snapshot_latencies.push(started.elapsed());
        if snapshot.len() != sessions {
            registry.shutdown();
            return Err(format!(
                "snapshot returned {} sessions, expected {sessions}",
                snapshot.len()
            ));
        }
    }

    let store = registry.store_snapshot();
    let store_json = serde_json::to_vec(&store).map_err(|error| format!("encode stress store: {error}"))?;
    let decoded: SessionStoreSnapshot = serde_json::from_slice(&store_json)
        .map_err(|error| format!("decode stress store: {error}"))?;
    let restored = SessionRegistry::from_store_with_domain_and_admission(
        decoded,
        Arc::new(SyntheticDomain::default()),
        AdmissionConfig::new(sessions, None),
    );
    let restored_sessions = restored.snapshot().len();
    let corrupt_store_rejected = serde_json::from_slice::<SessionStoreSnapshot>(
        &store_json[..store_json.len().saturating_sub(1)],
    )
    .is_err();
    let recovery = RecoverySummary {
        store_round_trip: restored_sessions == sessions,
        corrupt_store_rejected,
        restored_sessions,
    };
    restored.shutdown();

    let event_queue_depth = events.try_iter().count();
    let dropped_events = registry.admission_snapshot().dropped_events;
    let max_scrollback_bytes = store
        .sessions
        .iter()
        .map(|session| session.scrollback.len())
        .max()
        .unwrap_or_default();
    let hooks = LatencySummary::from_durations(hook_latencies);
    let snapshots = LatencySummary::from_durations(snapshot_latencies);
    let gates = StressGates {
        startup_under_budget: duration_from_ms(startup_ms) <= STARTUP_BUDGET,
        hooks_under_budget: duration_from_ms(hooks.p95_ms) <= HOOK_P95_BUDGET,
        snapshots_under_budget: duration_from_ms(snapshots.p95_ms) <= SNAPSHOT_P95_BUDGET,
        scrollback_bounded: max_scrollback_bytes <= crate::registry::MAX_SCROLLBACK_BYTES,
        event_queue_bounded: event_queue_depth <= SUBSCRIBER_QUEUE_CAPACITY,
        recovery_proven: recovery.store_round_trip && recovery.corrupt_store_rejected,
        all_pass: false,
    };
    let all_pass = gates.startup_under_budget
        && gates.hooks_under_budget
        && gates.snapshots_under_budget
        && gates.scrollback_bounded
        && gates.event_queue_bounded
        && gates.recovery_proven;
    let gates = StressGates { all_pass, ..gates };

    registry.shutdown();
    Ok(FleetStressReport {
        sessions,
        events: sessions * events_per_session,
        startup_ms,
        hooks,
        snapshots,
        max_scrollback_bytes,
        store_bytes: store_json.len(),
        event_queue_depth,
        dropped_events,
        recovery,
        gates,
    })
}

impl LatencySummary {
    fn from_durations(mut values: Vec<Duration>) -> Self {
        values.sort_unstable();
        let milliseconds = values
            .iter()
            .map(|value| elapsed_ms(*value))
            .collect::<Vec<_>>();
        let percentile = |percent: usize| -> f64 {
            if milliseconds.is_empty() {
                return 0.0;
            }
            let index = ((milliseconds.len() - 1) * percent).div_ceil(100);
            milliseconds[index.min(milliseconds.len() - 1)]
        };
        Self {
            samples: milliseconds.len(),
            min_ms: milliseconds.first().copied().unwrap_or(0.0),
            median_ms: percentile(50),
            p95_ms: percentile(95),
            max_ms: milliseconds.last().copied().unwrap_or(0.0),
        }
    }
}

fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn duration_from_ms(milliseconds: f64) -> Duration {
    Duration::from_secs_f64((milliseconds / 1000.0).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thirty_session_profile_stays_bounded_and_recovers() {
        let report = run(DEFAULT_SESSIONS, DEFAULT_EVENTS_PER_SESSION).expect("stress profile");
        assert_eq!(report.sessions, DEFAULT_SESSIONS);
        assert_eq!(report.events, DEFAULT_SESSIONS * DEFAULT_EVENTS_PER_SESSION);
        assert!(report.gates.all_pass, "{report:?}");
        assert!(report.dropped_events > 0, "the subscriber cap was not exercised");
        assert_eq!(report.recovery.restored_sessions, DEFAULT_SESSIONS);
        assert!(report.max_scrollback_bytes <= crate::registry::MAX_SCROLLBACK_BYTES);
    }

    #[test]
    fn latency_percentiles_are_deterministic_for_empty_and_odd_samples() {
        assert_eq!(LatencySummary::from_durations(Vec::new()).p95_ms, 0.0);
        let summary = LatencySummary::from_durations(vec![
            Duration::from_millis(1),
            Duration::from_millis(3),
            Duration::from_millis(5),
        ]);
        assert_eq!(summary.median_ms, 3.0);
        assert_eq!(summary.p95_ms, 5.0);
    }
}
