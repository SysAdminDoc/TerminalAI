//! Rust-owned session lifetime and fleet state.
//!
//! The GUI is intentionally not the owner of a process. A [`SessionRegistry`]
//! keeps the launch specification, live pty, bounded scrollback, focus and
//! fleet metadata together, then publishes small events to any interested
//! shell. This makes closing or reloading a view harmless to live agents.

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, BinaryHeap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::agent::{AgentBinary, Origin};
use crate::app_server::{AgentEvent, AppServerEvent};
use crate::diagnostics::{LogEntry, StatusSource};
use crate::environment::{self, EnvironmentError, EnvironmentSpec};
use crate::grid::{TerminalGrid, TerminalGridSnapshot};
use crate::hooks::{HookEvent, HookNotification, HookSignal};
use crate::launch::{LaunchError, LaunchSpec, ResolvedCommand, Resume};
use crate::notification::{NotificationCenter, NotificationChange, NotificationEvent};
use crate::pty::{PtyError, PtySession, PtySize};
use crate::review::{collect_reviews, ReviewItem};
use crate::session::{
    fleet_order, RestartDecision, Session, SessionId, SessionPhase, SessionStatus,
};
use crate::store::{ArchivedSession, SessionStoreSnapshot, StoredSession};

/// Maximum output retained per session in memory. The future daemon can spill
/// older bytes to disk without changing the registry-facing API.
pub const MAX_SCROLLBACK_BYTES: usize = 512 * 1024;
const MAX_LAST_LINE_BYTES: usize = 8 * 1024;
const SUBSCRIBER_QUEUE_CAPACITY: usize = 256;
/// Minimum gap between Git branch lookups for one session. Hooks fire per tool
/// call; a branch changes far less often than that.
const BRANCH_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_LIVE_SESSIONS: usize = 3;
pub const DEFAULT_SESSION_BUDGET_USD: f64 = 5.0;

fn session_span(
    id: &SessionId,
    agent: crate::agent::Agent,
    cwd: &std::path::Path,
) -> tracing::Span {
    tracing::info_span!(
        "terminalai.session",
        session_id = %id,
        agent = agent.command_name(),
        cwd = %cwd.display(),
    )
}

/// Admission limits are owned by the daemon but kept in the registry so every
/// process launch, including automatic restarts, observes the same cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdmissionConfig {
    pub max_live_sessions: usize,
    /// Applied to Claude launches that did not supply an explicit cap. Codex
    /// has no equivalent launcher flag and therefore leaves this unused.
    pub default_budget_usd: Option<f64>,
}

impl AdmissionConfig {
    pub fn new(max_live_sessions: usize, default_budget_usd: Option<f64>) -> Self {
        Self {
            max_live_sessions: max_live_sessions.max(1),
            default_budget_usd: default_budget_usd
                .filter(|value| value.is_finite() && *value >= 0.0),
        }
    }

    /// Read daemon-wide limits without introducing a second config file.
    /// TERMINALAI_DEFAULT_BUDGET_USD=none disables the Claude default cap.
    pub fn from_environment() -> Result<Self, String> {
        let max_live_sessions = std::env::var("TERMINALAI_MAX_LIVE_SESSIONS")
            .ok()
            .map(|value| {
                value.parse::<usize>().map_err(|_| {
                    "TERMINALAI_MAX_LIVE_SESSIONS must be a positive integer".to_string()
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_MAX_LIVE_SESSIONS);
        let default_budget_usd = match std::env::var("TERMINALAI_DEFAULT_BUDGET_USD") {
            Ok(value)
                if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("off") =>
            {
                None
            }
            Ok(value) => Some(value.parse::<f64>().map_err(|_| {
                "TERMINALAI_DEFAULT_BUDGET_USD must be a non-negative decimal or 'none'".to_string()
            })?),
            Err(_) => Some(DEFAULT_SESSION_BUDGET_USD),
        };
        let config = Self::new(max_live_sessions, default_budget_usd);
        if config.max_live_sessions != max_live_sessions {
            return Err("TERMINALAI_MAX_LIVE_SESSIONS must be at least 1".into());
        }
        if config.default_budget_usd != default_budget_usd {
            return Err("TERMINALAI_DEFAULT_BUDGET_USD must be finite and non-negative".into());
        }
        Ok(config)
    }
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LIVE_SESSIONS, None)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdmissionSnapshot {
    pub max_live_sessions: usize,
    pub live_sessions: usize,
    pub queued_sessions: usize,
    pub aggregate_cost_usd: f64,
    /// Nonblocking event delivery drops since daemon start. Output and row
    /// updates are deliberately lossy when a subscriber is stalled; clients
    /// can recover authoritative state with Snapshot/Reattach.
    #[serde(default)]
    pub dropped_events: u64,
    /// Which price table any reported cost was computed against. Shown beside
    /// the figure so a stale table is visible rather than assumed current.
    #[serde(default)]
    pub pricing_version: String,
    /// How many sessions actually reported a cost. Zero means the fleet spend is
    /// unknown, not zero.
    #[serde(default)]
    pub sessions_reporting_cost: usize,
}

/// Events are deliberately coarse: a view can rebuild its rows from a session
/// update and only the focused pane needs to consume output bytes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RegistryEvent {
    SessionUpdated { session: Session },
    Notification { event: NotificationEvent },
    AgentEvent { event: AgentEvent },
    Log { entry: LogEntry },
    Output { id: SessionId, data: Vec<u8> },
    SessionRemoved { id: SessionId },
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    #[error(transparent)]
    Pty(#[from] PtyError),
    #[error("session does not exist: {0}")]
    Missing(SessionId),
    #[error("at most three sessions may be pinned")]
    PinLimit,
    #[error("cannot launch {requested} with a {binary} executable")]
    AgentMismatch {
        requested: &'static str,
        binary: &'static str,
    },
    #[error("session is still running: {0}")]
    StillRunning(SessionId),
    #[error("session has no native resume id: {0}")]
    NoResumeId(SessionId),
    #[error("could not start {phase} worker: {cause}")]
    WorkerSpawn { phase: &'static str, cause: String },
    #[error("session is no longer running: {0}")]
    NotRunning(SessionId),
    #[error("cannot record a review for {0}: its working tree could not be read")]
    ReviewStateUnavailable(SessionId),
}

struct Entry {
    session: Session,
    spec: LaunchSpec,
    command: ResolvedCommand,
    pty: Option<Arc<PtySession>>,
    scrollback: RingBuffer,
    grid: TerminalGrid,
    generation: u64,
    stop_requested: bool,
    teardown_done: bool,
    span: tracing::Span,
    /// When the branch was last read from Git. Hooks fire per tool call, so the
    /// lookup is rate limited rather than run on every event.
    branch_checked: Option<Instant>,
}

struct State {
    next_id: u64,
    focused: Option<SessionId>,
    entries: BTreeMap<SessionId, Entry>,
    archives: Vec<ArchivedSession>,
    queue: VecDeque<SessionId>,
    admission: AdmissionConfig,
    notifications: NotificationCenter,
    subscribers: Vec<SyncSender<RegistryEvent>>,
}

#[derive(Debug, Eq, PartialEq)]
struct RestartTask {
    due: Instant,
    sequence: u64,
    id: SessionId,
    generation: u64,
}

struct TeardownTask {
    id: SessionId,
    generation: u64,
    final_phase: SessionPhase,
    restart: Option<(u64, Duration)>,
    cwd: std::path::PathBuf,
    spec: EnvironmentSpec,
    ports: Vec<u16>,
    span: tracing::Span,
}

impl Ord for RestartTask {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        other
            .due
            .cmp(&self.due)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for RestartTask {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

struct Inner {
    state: Mutex<State>,
    dropped_events: AtomicU64,
    restart_tx: Sender<RestartTask>,
    restart_sequence: AtomicU64,
}

impl Inner {
    fn is_poisoned(&self) -> bool {
        self.state.is_poisoned()
    }
}

/// A panic while holding the registry lock must not turn the daemon into a
/// second, silent failure. Recover the guard so callers can inspect or shut
/// down the fleet, while the daemon checks [`SessionRegistry::is_poisoned`]
/// and returns an explicit error for stateful requests.
fn lock_state(inner: &Inner) -> MutexGuard<'_, State> {
    inner
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Owns all sessions and publishes changes without requiring a UI toolkit.
#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<Inner>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    pub fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }

    pub fn new() -> Self {
        Self::with_admission(AdmissionConfig::default())
    }

    pub fn with_admission(admission: AdmissionConfig) -> Self {
        let (restart_tx, restart_rx) = mpsc::channel();
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                next_id: 1,
                focused: None,
                entries: BTreeMap::new(),
                archives: Vec::new(),
                queue: VecDeque::new(),
                admission,
                notifications: NotificationCenter::default(),
                subscribers: Vec::new(),
            }),
            dropped_events: AtomicU64::new(0),
            restart_tx,
            restart_sequence: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&inner);
        let _ = thread::Builder::new()
            .name("terminalai-restart-scheduler".into())
            .spawn(move || restart_scheduler_loop(restart_rx, weak));
        Self { inner }
    }

    /// Rehydrate rows from the last durable snapshot without starting any
    /// agent automatically. A daemon restart cannot steal a live ConPTY from
    /// the old process, so persisted rows are offered as explicit revives.
    pub fn from_store(snapshot: SessionStoreSnapshot) -> Self {
        Self::from_store_with_admission(snapshot, AdmissionConfig::default())
    }

    pub fn from_store_with_admission(
        snapshot: SessionStoreSnapshot,
        admission: AdmissionConfig,
    ) -> Self {
        let registry = Self::with_admission(admission);
        let mut state = lock_state(&registry.inner);
        let archives = snapshot.archives;
        for archive in &archives {
            state.next_id = state.next_id.max(next_sequence(&archive.id));
        }
        state.archives = archives;
        for stored in snapshot.sessions {
            let StoredSession {
                mut session,
                spec,
                command,
                scrollback: bytes,
            } = stored;
            let id = session.id.clone();
            if session.ports.is_empty() && spec.environment.port_count > 0 {
                session.ports = spec
                    .environment
                    .ports_for_session(&id.0)
                    .unwrap_or_default();
            }
            let exit_code = session.last_exit_code;
            session.mark_resurrectable_at_from(exit_code, SystemTime::now(), StatusSource::Restore);
            let mut scrollback = RingBuffer::default();
            scrollback.push(&bytes);
            let mut grid = TerminalGrid::default();
            grid.advance(&bytes);
            state.next_id = state.next_id.max(next_sequence(&id));
            let span = session_span(&id, session.agent, &session.cwd);
            state.entries.insert(
                id,
                Entry {
                    session,
                    spec,
                    command,
                    pty: None,
                    scrollback,
                    grid,
                    generation: 1,
                    stop_requested: false,
                    branch_checked: None,
                    teardown_done: true,
                    span,
                },
            );
        }
        drop(state);
        registry
    }

    pub fn admission_snapshot(&self) -> AdmissionSnapshot {
        let state = lock_state(&self.inner);
        AdmissionSnapshot {
            max_live_sessions: state.admission.max_live_sessions,
            live_sessions: admitted_count(&state),
            queued_sessions: state.queue.len(),
            aggregate_cost_usd: state
                .entries
                .values()
                .filter_map(|entry| entry.session.cost_usd)
                .sum(),
            dropped_events: self.inner.dropped_events.load(Ordering::Relaxed),
            pricing_version: crate::transcript::PricingTable::vendored().version.clone(),
            sessions_reporting_cost: state
                .entries
                .values()
                .filter(|entry| entry.session.cost_usd.is_some())
                .count(),
        }
    }

    /// Add a drop observed by a downstream bounded event queue to the same
    /// diagnostic counter used by registry subscribers.
    pub fn record_dropped_event(&self) {
        self.inner.dropped_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn is_queued(&self, id: &SessionId) -> Result<bool, RegistryError> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .map(|entry| entry.session.status == SessionStatus::Queued)
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    /// Capture only serializable state; live PTY handles and parsed grids are
    /// intentionally reconstructed on restore.
    pub fn store_snapshot(&self) -> SessionStoreSnapshot {
        let state = lock_state(&self.inner);
        SessionStoreSnapshot {
            schema_version: crate::store::SESSION_STORE_SCHEMA_VERSION,
            sessions: state
                .entries
                .values()
                .map(|entry| StoredSession {
                    session: entry.session.clone(),
                    spec: entry.spec.clone(),
                    command: entry.command.clone(),
                    scrollback: entry.scrollback.to_vec(),
                })
                .collect(),
            archives: state.archives.clone(),
        }
    }

    /// Subscribe to pushed changes. Closed receivers are removed automatically.
    pub fn subscribe(&self) -> Receiver<RegistryEvent> {
        let (sender, receiver) = mpsc::sync_channel(SUBSCRIBER_QUEUE_CAPACITY);
        lock_state(&self.inner).subscribers.push(sender);
        receiver
    }

    /// Current rows, sorted with attention first and longest dwell time next.
    pub fn snapshot(&self) -> Vec<Session> {
        let state = lock_state(&self.inner);
        let mut sessions: Vec<_> = state
            .entries
            .values()
            .map(|entry| entry.session.clone())
            .collect();
        sessions.sort_by(fleet_order);
        sessions
    }

    /// Read a bounded, daemon-owned review snapshot without holding the
    /// registry lock while Git inspects each session directory.
    pub fn review_snapshot(&self) -> Vec<ReviewItem> {
        let sessions: Vec<_> = {
            let state = lock_state(&self.inner);
            state
                .entries
                .values()
                .map(|entry| entry.session.clone())
                .collect()
        };
        let mut reviews = collect_reviews(sessions);
        reviews.sort_by(|a, b| {
            b.review_cost
                .cmp(&a.review_cost)
                .then_with(|| b.conflicts.len().cmp(&a.conflicts.len()))
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        reviews
    }

    pub fn focused(&self) -> Option<SessionId> {
        lock_state(&self.inner).focused.clone()
    }

    /// Spawn a session and immediately make it visible to subscribers.
    pub fn launch(
        &self,
        mut spec: LaunchSpec,
        binary: AgentBinary,
    ) -> Result<SessionId, RegistryError> {
        if spec.agent != binary.agent {
            return Err(RegistryError::AgentMismatch {
                requested: spec.agent.command_name(),
                binary: binary.agent.command_name(),
            });
        }
        let admission = lock_state(&self.inner).admission;
        if spec.agent == crate::agent::Agent::Claude && spec.max_budget_usd.is_none() {
            spec.max_budget_usd = admission.default_budget_usd;
        }
        let command = spec.resolve(&binary)?;
        // Read before the lock: this shells out to Git. Resolving here rather
        // than waiting for the first hook means the branch is on the row even
        // for an agent with no hook installed.
        let branch = crate::review::current_branch(&spec.cwd);
        let branch_checked = Some(Instant::now());
        let (id, queued) = {
            let mut state = lock_state(&self.inner);
            let id = SessionId::new(state.next_id);
            state.next_id = state.next_id.saturating_add(1);
            let queued = admitted_count(&state) >= state.admission.max_live_sessions;
            let mut session = Session::new(id.clone(), &spec);
            session.branch = branch;
            if queued {
                session.mark_queued_at(SystemTime::now());
                state.queue.push_back(id.clone());
            } else {
                session.mark_preparing();
            }
            let span = session_span(&id, session.agent, &session.cwd);
            tracing::info!(parent: &span, queued, "session launch admitted");
            state.entries.insert(
                id.clone(),
                Entry {
                    session,
                    spec: spec.clone(),
                    command: command.clone(),
                    pty: None,
                    scrollback: RingBuffer::default(),
                    grid: TerminalGrid::default(),
                    generation: 1,
                    stop_requested: false,
                    branch_checked,
                    teardown_done: true,
                    span,
                },
            );
            (id, queued)
        };
        self.emit_session(&id);
        if queued {
            return Ok(id);
        }

        if let Err(error) = self.start_entry(&id, command, 1) {
            self.remove_entry(&id);
            return Err(error);
        }
        Ok(id)
    }

    pub fn write(&self, id: &SessionId, bytes: &[u8]) -> Result<(), RegistryError> {
        let pty = self.pty(id)?;
        pty.write(bytes).map_err(RegistryError::from)
    }

    /// Focus a live process and return the bounded raw tail for renderer
    /// replay. The GUI uses this when it reconnects to the already-running
    /// daemon after a window reload.
    pub fn reattach(&self, id: &SessionId) -> Result<Vec<u8>, RegistryError> {
        let pty = self.pty(id)?;
        if pty.try_wait()?.is_some() {
            return Err(RegistryError::NotRunning(id.clone()));
        }
        self.focus(Some(id.clone()))?;
        self.scrollback(id)
    }

    /// Explicitly revive a non-running row through its native resume command.
    /// Automatic restart never calls this path.
    pub fn revive(&self, id: &SessionId) -> Result<SessionId, RegistryError> {
        let (command, generation, queued) = {
            let mut state = lock_state(&self.inner);
            let admission_full = admitted_count(&state) >= state.admission.max_live_sessions;
            let entry = state
                .entries
                .get_mut(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?;
            if entry.pty.is_some()
                || matches!(
                    entry.session.phase,
                    SessionPhase::Preparing | SessionPhase::Starting | SessionPhase::TearingDown
                )
            {
                return Err(RegistryError::StillRunning(id.clone()));
            }
            let resume_id = entry
                .session
                .resume_id
                .clone()
                .ok_or_else(|| RegistryError::NoResumeId(id.clone()))?;
            let mut spec = entry.spec.clone();
            spec.resume = Resume::Session(resume_id);
            spec.initial_prompt = None;
            let binary = AgentBinary {
                agent: spec.agent,
                path: entry.command.program.clone(),
                origin: Origin::Configured,
            };
            let command = spec.resolve(&binary)?;
            entry.spec = spec;
            entry.command = command.clone();
            entry.stop_requested = false;
            entry.generation = entry.generation.saturating_add(1);
            let generation = entry.generation;
            let queued = admission_full;
            if queued {
                entry.session.mark_queued_at(SystemTime::now());
                state.queue.push_back(id.clone());
            } else {
                entry.session.begin_manual_revive_at(SystemTime::now());
                entry.session.mark_preparing();
            }
            (command, generation, queued)
        };
        self.emit_session(id);
        if queued {
            return Ok(id.clone());
        }

        if let Err(error) = self.start_entry(id, command, generation) {
            let mut state = lock_state(&self.inner);
            if let Some(entry) = state.entries.get_mut(id) {
                if entry.generation == generation {
                    entry.session.mark_resurrectable_at_from(
                        None,
                        SystemTime::now(),
                        StatusSource::Manual,
                    );
                }
            }
            self.emit_session(id);
            return Err(error);
        }
        Ok(id.clone())
    }

    /// Remove a stopped row from the live fleet while preserving only the
    /// layout, cwd and exact command in the durable archive.
    pub fn archive(&self, id: &SessionId) -> Result<ArchivedSession, RegistryError> {
        let (archived, notifications) = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get(id) else {
                return Err(RegistryError::Missing(id.clone()));
            };
            if entry.pty.is_some() {
                return Err(RegistryError::StillRunning(id.clone()));
            }
            let entry = state.entries.remove(id).expect("entry checked above");
            state.queue.retain(|queued| queued != id);
            if state.focused.as_ref() == Some(id) {
                state.focused = None;
            }
            let archived = ArchivedSession::from_session(&entry.session, &entry.command);
            state.archives.retain(|item| item.id != *id);
            state.archives.push(archived.clone());
            let notifications = state.notifications.retract_session(id);
            (archived, notifications)
        };
        self.emit(RegistryEvent::SessionRemoved { id: id.clone() });
        self.emit_notification_changes(notifications);
        self.drain_queue();
        Ok(archived)
    }

    pub fn resize(&self, id: &SessionId, size: PtySize) -> Result<(), RegistryError> {
        let pty = self.pty(id)?;
        pty.resize(size).map_err(RegistryError::from)?;
        let mut state = lock_state(&self.inner);
        let entry = state
            .entries
            .get_mut(id)
            .ok_or_else(|| RegistryError::Missing(id.clone()))?;
        entry.grid.resize(size.rows, size.cols);
        Ok(())
    }

    pub fn kill(&self, id: &SessionId) -> Result<(), RegistryError> {
        let (pty, generation) = {
            let mut state = lock_state(&self.inner);
            let entry = state
                .entries
                .get_mut(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?;
            entry.stop_requested = true;
            let Some(pty) = entry.pty.clone() else {
                if matches!(
                    entry.session.phase,
                    SessionPhase::Preparing | SessionPhase::Starting
                ) {
                    let previous_status = entry.session.status;
                    let previous_state_since = entry.session.state_since;
                    entry.generation = entry.generation.saturating_add(1);
                    let now = SystemTime::now();
                    entry
                        .session
                        .mark_resurrectable_at_from(None, now, StatusSource::Manual);
                    let session = entry.session.clone();
                    let notifications = state.notifications.observe(
                        &session,
                        previous_status,
                        previous_state_since,
                        now,
                    );
                    drop(state);
                    self.emit(RegistryEvent::SessionUpdated { session });
                    self.emit_notification_changes(notifications);
                    self.drain_queue();
                    return Ok(());
                }
                if entry.session.status == SessionStatus::Queued {
                    let previous_status = entry.session.status;
                    let previous_state_since = entry.session.state_since;
                    entry.generation = entry.generation.saturating_add(1);
                    let now = SystemTime::now();
                    entry
                        .session
                        .mark_resurrectable_at_from(None, now, StatusSource::Manual);
                    let session = entry.session.clone();
                    let notifications = state.notifications.observe(
                        &session,
                        previous_status,
                        previous_state_since,
                        now,
                    );
                    state.queue.retain(|queued| queued != id);
                    drop(state);
                    self.emit(RegistryEvent::SessionUpdated { session });
                    self.emit_notification_changes(notifications);
                    self.drain_queue();
                    return Ok(());
                }
                if entry.session.phase == SessionPhase::Backoff {
                    let previous_status = entry.session.status;
                    let previous_state_since = entry.session.state_since;
                    entry.generation = entry.generation.saturating_add(1);
                    let now = SystemTime::now();
                    entry
                        .session
                        .mark_resurrectable_at_from(None, now, StatusSource::Manual);
                    let session = entry.session.clone();
                    let notifications = state.notifications.observe(
                        &session,
                        previous_status,
                        previous_state_since,
                        now,
                    );
                    drop(state);
                    self.emit(RegistryEvent::SessionUpdated { session });
                    self.emit_notification_changes(notifications);
                    self.drain_queue();
                    return Ok(());
                }
                return Err(RegistryError::Missing(id.clone()));
            };
            (pty, entry.generation)
        };
        if let Err(error) = pty.kill() {
            let mut state = lock_state(&self.inner);
            if let Some(entry) = state.entries.get_mut(id) {
                entry.stop_requested = false;
            }
            return Err(error.into());
        }
        self.mark_process_exit(id, generation, None);
        Ok(())
    }

    pub fn mark_read(&self, id: &SessionId) -> Result<(), RegistryError> {
        self.update(id, |session| session.unread = false)
    }

    /// Record that the operator has reviewed this session *as it stands now*.
    ///
    /// The current diff state is fingerprinted at mark time and stored with the
    /// mark, so the acknowledgement retires by itself as soon as the agent
    /// touches another file. Marking a session whose repository cannot be read
    /// yields an empty digest, which never matches — an unreadable tree stays
    /// unreviewed rather than becoming permanently acknowledged.
    pub fn mark_reviewed(&self, id: &SessionId) -> Result<(), RegistryError> {
        let session = {
            let state = lock_state(&self.inner);
            state
                .entries
                .get(id)
                .map(|entry| entry.session.clone())
                .ok_or_else(|| RegistryError::Missing(id.clone()))?
        };
        // Collected outside the lock: this shells out to Git.
        let digest = crate::review::collect_review(&session).state_digest;
        if digest.is_empty() {
            return Err(RegistryError::ReviewStateUnavailable(id.clone()));
        }
        self.update(id, |session| session.reviewed_digest = Some(digest))
    }

    /// Apply a normalized Claude/Codex hook to the matching live session.
    ///
    /// A hook may arrive before the agent has written its native id anywhere
    /// else, so SessionStart first falls back to the newest starting session
    /// for the same agent and working directory. Unknown sessions are ignored:
    /// hooks from agents launched outside TerminalAI must not fabricate rows or
    /// delete state.
    pub fn apply_hook(&self, event: HookEvent) -> bool {
        self.apply_hook_from(event, StatusSource::Hook)
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

    fn apply_hook_from(&self, event: HookEvent, source: StatusSource) -> bool {
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
                                && Some(session_id) == entry.session.resume_id.as_deref()
                        })
                        .map(|(id, _)| id.clone())
                })
                .or_else(|| {
                    state
                        .entries
                        .iter()
                        .rev()
                        .find(|(_, entry)| {
                            entry.session.agent == event.agent
                                && event.cwd.as_ref() == Some(&entry.session.cwd)
                                && entry.session.resume_id.is_none()
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
            if let Some(resume_id) = event.session_id {
                entry.session.resume_id = Some(resume_id);
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
                HookSignal::Stop => entry.session.set_status_from(SessionStatus::Idle, source),
                HookSignal::PreToolUse => entry
                    .session
                    .set_status_from(SessionStatus::Working, source),
                HookSignal::PostToolUse => entry
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
            }
            let session = entry.session.clone();
            let notifications = state.notifications.observe(
                &session,
                previous_status,
                previous_state_since,
                SystemTime::now(),
            );
            (session, notifications)
        };
        self.emit(RegistryEvent::SessionUpdated { session });
        self.emit_notification_changes(notifications);
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
        )
    }

    fn has_native_session(&self, agent: crate::agent::Agent, native_id: &str) -> bool {
        let state = lock_state(&self.inner);
        state.entries.values().any(|entry| {
            entry.session.agent == agent && entry.session.resume_id.as_deref() == Some(native_id)
        })
    }

    pub fn focus(&self, id: Option<SessionId>) -> Result<(), RegistryError> {
        if let Some(id) = &id {
            self.require(id)?;
        }
        {
            let mut state = lock_state(&self.inner);
            state.focused = id.clone();
            if let Some(id) = &id {
                if let Some(entry) = state.entries.get_mut(id) {
                    entry.session.unread = false;
                }
            }
        }
        if let Some(id) = id {
            self.emit_session(&id);
        }
        Ok(())
    }

    pub fn toggle_pin(&self, id: &SessionId) -> Result<bool, RegistryError> {
        let pinned = {
            let mut state = lock_state(&self.inner);
            let currently_pinned = state
                .entries
                .get(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?
                .session
                .pinned;
            if !currently_pinned
                && state
                    .entries
                    .values()
                    .filter(|entry| entry.session.pinned)
                    .count()
                    >= 3
            {
                return Err(RegistryError::PinLimit);
            }
            let entry = state.entries.get_mut(id).expect("checked above");
            entry.session.pinned = !currently_pinned;
            entry.session.pinned
        };
        self.emit_session(id);
        Ok(pinned)
    }

    pub fn scrollback(&self, id: &SessionId) -> Result<Vec<u8>, RegistryError> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .map(|entry| entry.scrollback.to_vec())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    /// Return the parsed terminal state held for a background or pinned pane.
    /// The focused browser renderer does not need this path: it resets once and
    /// replays the raw bounded ring returned by [`Self::scrollback`].
    pub fn grid_snapshot(&self, id: &SessionId) -> Result<TerminalGridSnapshot, RegistryError> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .map(|entry| entry.grid.snapshot())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    pub fn spec(&self, id: &SessionId) -> Result<LaunchSpec, RegistryError> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .map(|entry| entry.spec.clone())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    pub fn shutdown(&self) {
        let sessions: Vec<_> = {
            let mut state = lock_state(&self.inner);
            state
                .entries
                .values_mut()
                .filter_map(|entry| {
                    entry.stop_requested = true;
                    let pty = entry.pty.clone()?;
                    Some((entry.session.id.clone(), entry.generation, pty))
                })
                .collect()
        };
        for (id, generation, pty) in sessions {
            let _ = pty.kill();
            self.mark_process_exit(&id, generation, None);
        }
    }

    fn pty(&self, id: &SessionId) -> Result<Arc<PtySession>, RegistryError> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .and_then(|entry| entry.pty.clone())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    fn require(&self, id: &SessionId) -> Result<(), RegistryError> {
        let state = lock_state(&self.inner);
        if state.entries.contains_key(id) {
            Ok(())
        } else {
            Err(RegistryError::Missing(id.clone()))
        }
    }

    fn update(&self, id: &SessionId, f: impl FnOnce(&mut Session)) -> Result<(), RegistryError> {
        {
            let mut state = lock_state(&self.inner);
            let entry = state
                .entries
                .get_mut(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?;
            f(&mut entry.session);
        }
        self.emit_session(id);
        Ok(())
    }

    fn spawn_pty(
        &self,
        id: &SessionId,
        command: &ResolvedCommand,
        generation: u64,
        environment: &[(String, String)],
    ) -> Result<Arc<PtySession>, RegistryError> {
        let weak = Arc::downgrade(&self.inner);
        let callback_id = id.clone();
        let span = self.span_for(id);
        let pty = PtySession::spawn_with_environment(
            command,
            crate::pty::default_size(),
            environment,
            move |chunk| {
                let _entered = span.enter();
                if let Some(inner) = weak.upgrade() {
                    handle_output(&inner, &callback_id, generation, chunk);
                }
            },
        )?;
        Ok(Arc::new(pty))
    }

    fn emit_session(&self, id: &SessionId) {
        let session = {
            let state = lock_state(&self.inner);
            state.entries.get(id).map(|entry| entry.session.clone())
        };
        if let Some(session) = session {
            self.emit(RegistryEvent::SessionUpdated { session });
        }
    }

    fn emit_notification_changes(&self, changes: Vec<NotificationChange>) {
        for change in changes {
            if let Some(event) = change.into_event() {
                self.emit(RegistryEvent::Notification { event });
            }
        }
    }

    fn start_entry(
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
        let (cwd, environment_spec, ports) = self.runtime_environment(id)?;
        tracing::debug!("preparing session environment");
        let environment = environment::variables(&id.0, &ports);
        if let Err(error) = environment::run_setup(&environment_spec, &id.0, &cwd, &ports) {
            let _ = environment::run_teardown(&environment_spec, &id.0, &cwd, &ports);
            return Err(error.into());
        }
        if !self.start_is_current(id, generation) {
            let _ = environment::run_teardown(&environment_spec, &id.0, &cwd, &ports);
            return Err(RegistryError::NotRunning(id.clone()));
        }
        let pty = match self.spawn_pty(id, command, generation, &environment) {
            Ok(pty) => pty,
            Err(error) => {
                let _ = environment::run_teardown(&environment_spec, &id.0, &cwd, &ports);
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
            let _ = environment::run_teardown(&environment_spec, &id.0, &cwd, &ports);
            return Err(RegistryError::NotRunning(id.clone()));
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
        self.emit(RegistryEvent::SessionUpdated { session });
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
    ) -> Result<(std::path::PathBuf, EnvironmentSpec, Vec<u16>), RegistryError> {
        let state = lock_state(&self.inner);
        let entry = state
            .entries
            .get(id)
            .ok_or_else(|| RegistryError::Missing(id.clone()))?;
        Ok((
            entry.session.cwd.clone(),
            entry.spec.environment.clone(),
            entry.session.ports.clone(),
        ))
    }

    fn drain_queue(&self) {
        loop {
            let (id, command, generation) = {
                let mut state = lock_state(&self.inner);
                if admitted_count(&state) >= state.admission.max_live_sessions {
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
                self.emit(RegistryEvent::SessionUpdated { session });
            }
        }
    }

    fn emit(&self, event: RegistryEvent) {
        let mut state = lock_state(&self.inner);
        let mut dropped = 0;
        state
            .subscribers
            .retain(|subscriber| match subscriber.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    dropped += 1;
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            });
        if dropped != 0 {
            self.inner
                .dropped_events
                .fetch_add(dropped, Ordering::Relaxed);
        }
    }

    fn remove_entry(&self, id: &SessionId) {
        let (removed, notifications) = {
            let mut state = lock_state(&self.inner);
            if state.focused.as_ref() == Some(id) {
                state.focused = None;
            }
            let removed = state.entries.remove(id).is_some();
            let notifications = if removed {
                state.notifications.retract_session(id)
            } else {
                Vec::new()
            };
            (removed, notifications)
        };
        if removed {
            self.emit(RegistryEvent::SessionRemoved { id: id.clone() });
        }
        self.emit_notification_changes(notifications);
    }

    fn mark_process_exit(&self, id: &SessionId, generation: u64, exit_code: Option<u32>) {
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
                    RestartDecision::Failed => None,
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
        self.emit(RegistryEvent::SessionUpdated { session });
        self.emit_notification_changes(notifications);
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
                let error = environment::run_teardown(
                    &worker_spec,
                    &worker_id.0,
                    &worker_cwd,
                    &worker_ports,
                )
                .err()
                .map(|error| error.to_string());
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
        self.emit(RegistryEvent::SessionUpdated { session });
        self.complete_process_exit(id, restart);
    }

    fn complete_process_exit(&self, id: &SessionId, restart: Option<(u64, Duration)>) {
        self.drain_queue();
        if let Some((generation, delay)) = restart {
            self.schedule_restart(id.clone(), generation, delay);
        }
    }

    fn mark_process_unknown(&self, id: &SessionId, generation: u64) {
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
        self.emit(RegistryEvent::SessionUpdated { session });
    }

    fn schedule_restart(&self, id: SessionId, generation: u64, delay: Duration) {
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
            let admission_full = admitted_count(&state) >= state.admission.max_live_sessions;
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
                    RestartDecision::Failed => None,
                };
            let session = entry.session.clone();
            let notifications =
                state
                    .notifications
                    .observe(&session, previous_status, previous_state_since, now);
            (restart, session, notifications)
        };
        self.emit(RegistryEvent::SessionUpdated { session });
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
        self.emit(RegistryEvent::SessionUpdated { session });
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
        pty: Arc<PtySession>,
        generation: u64,
        span: tracing::Span,
    ) {
        let registry = self.clone();
        let _ = thread::Builder::new()
            .name(format!("terminalai-session-{id}"))
            .spawn(move || {
                let _entered = span.enter();
                if let Ok(status) = pty.wait_for_exit() {
                    registry.mark_process_exit(&id, generation, Some(status));
                    return;
                }
                Self::poll_until_exit(&registry, &id, &pty, generation);
            });
    }

    fn poll_until_exit(
        registry: &SessionRegistry,
        id: &SessionId,
        pty: &PtySession,
        generation: u64,
    ) {
        loop {
            match pty.try_wait() {
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

    fn span_for(&self, id: &SessionId) -> tracing::Span {
        lock_state(&self.inner)
            .entries
            .get(id)
            .map(|entry| entry.span.clone())
            .unwrap_or_else(tracing::Span::none)
    }
}

fn restart_scheduler_loop(receiver: Receiver<RestartTask>, inner: Weak<Inner>) {
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

        if let Some(task) = pending.peek() {
            match receiver.recv_timeout(task.due.saturating_duration_since(Instant::now())) {
                Ok(task) => pending.push(task),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match receiver.recv() {
                Ok(task) => pending.push(task),
                Err(_) => return,
            }
        }
    }
}

fn handle_output(inner: &Arc<Inner>, id: &SessionId, generation: u64, bytes: &[u8]) {
    let (send_output, session, notifications) = {
        let mut state = lock_state(inner);
        let focused = state.focused.as_ref() == Some(id);
        let Some(entry) = state.entries.get_mut(id) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        entry.scrollback.push(bytes);
        entry.grid.advance(bytes);
        if let Some(line) = entry.scrollback.last_line() {
            entry.session.set_last_line(&line);
        }
        let previous_status = entry.session.status;
        let previous_state_since = entry.session.state_since;
        if entry.session.status == SessionStatus::Starting {
            entry
                .session
                .set_status_from(SessionStatus::Idle, StatusSource::PtyOutput);
        }
        let session = entry.session.clone();
        let notifications = if previous_status != session.status {
            state.notifications.observe(
                &session,
                previous_status,
                previous_state_since,
                SystemTime::now(),
            )
        } else {
            Vec::new()
        };
        (focused || session.pinned, session, notifications)
    };
    if send_output {
        emit_inner(
            inner,
            RegistryEvent::Output {
                id: id.clone(),
                data: bytes.to_vec(),
            },
        );
    }
    emit_inner(inner, RegistryEvent::SessionUpdated { session });
    emit_notification_changes_inner(inner, notifications);
}

fn emit_notification_changes_inner(inner: &Arc<Inner>, changes: Vec<NotificationChange>) {
    for change in changes {
        if let Some(event) = change.into_event() {
            emit_inner(inner, RegistryEvent::Notification { event });
        }
    }
}

fn emit_inner(inner: &Arc<Inner>, event: RegistryEvent) {
    let mut state = lock_state(inner);
    let mut dropped = 0;
    state
        .subscribers
        .retain(|subscriber| match subscriber.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                dropped += 1;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    if dropped != 0 {
        inner.dropped_events.fetch_add(dropped, Ordering::Relaxed);
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

fn next_sequence(id: &SessionId) -> u64 {
    id.0.strip_prefix('s')
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1)
}

fn admitted_count(state: &State) -> usize {
    state
        .entries
        .values()
        .filter(|entry| entry.session.status.is_live())
        .count()
}

#[derive(Default)]
struct RingBuffer {
    bytes: VecDeque<u8>,
}

impl RingBuffer {
    fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= MAX_SCROLLBACK_BYTES {
            self.bytes.clear();
            self.bytes
                .extend(bytes[bytes.len() - MAX_SCROLLBACK_BYTES..].iter().copied());
            return;
        }
        self.bytes.extend(bytes.iter().copied());
        while self.bytes.len() > MAX_SCROLLBACK_BYTES {
            let _ = self.bytes.pop_front();
        }
    }

    fn to_vec(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    fn last_line(&self) -> Option<String> {
        let mut candidate_reversed = Vec::with_capacity(MAX_LAST_LINE_BYTES.min(self.bytes.len()));
        for byte in self.bytes.iter().rev().take(MAX_LAST_LINE_BYTES) {
            if matches!(byte, b'\r' | b'\n') {
                if let Some(line) = decode_last_line_candidate(&mut candidate_reversed) {
                    return Some(line);
                }
            } else {
                candidate_reversed.push(*byte);
            }
        }
        decode_last_line_candidate(&mut candidate_reversed)
    }
}

fn decode_last_line_candidate(candidate_reversed: &mut Vec<u8>) -> Option<String> {
    if candidate_reversed.is_empty() {
        return None;
    }
    candidate_reversed.reverse();
    let text = String::from_utf8_lossy(candidate_reversed);
    let line = (!text.trim().is_empty()).then(|| text.into_owned());
    candidate_reversed.clear();
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentBinary, Origin};
    use crate::app_server::{AppServerEvent, AppServerThreadStatus, AppServerTokenUsage};
    use crate::launch::spec_for;
    use std::path::{Path, PathBuf};

    #[test]
    fn ring_buffer_keeps_a_bounded_tail() {
        let mut ring = RingBuffer::default();
        ring.push(&vec![b'x'; MAX_SCROLLBACK_BYTES + 10]);
        assert_eq!(ring.to_vec().len(), MAX_SCROLLBACK_BYTES);
    }

    #[test]
    fn ring_buffer_finds_last_non_empty_line() {
        let mut ring = RingBuffer::default();
        ring.push(b"first\r\nlast\r\n");
        assert_eq!(ring.last_line().as_deref(), Some("last"));
    }

    #[test]
    fn ring_buffer_finds_last_line_across_chunks() {
        let mut ring = RingBuffer::default();
        ring.push(b"first\r");
        ring.push(b"\npart");
        ring.push(b"ial");
        assert_eq!(ring.last_line().as_deref(), Some("partial"));
    }

    #[test]
    fn ring_buffer_finds_partial_line_without_a_newline() {
        let mut ring = RingBuffer::default();
        ring.push(b"still typing");
        assert_eq!(ring.last_line().as_deref(), Some("still typing"));
    }

    #[test]
    fn ring_buffer_bounds_last_line_scan() {
        let mut ring = RingBuffer::default();
        ring.push(&vec![b'x'; MAX_LAST_LINE_BYTES + 1]);
        assert_eq!(
            ring.last_line().as_deref(),
            Some("x".repeat(MAX_LAST_LINE_BYTES).as_str())
        );
    }

    #[test]
    fn subscriber_queue_drops_newest_events_and_reports_the_count() {
        let registry = SessionRegistry::new();
        let events = registry.subscribe();
        let id = SessionId::new(1);
        let spec = spec_for(Agent::Claude, Path::new("."));
        let session = Session::new(id.clone(), &spec);

        for _ in 0..SUBSCRIBER_QUEUE_CAPACITY {
            registry.emit(RegistryEvent::SessionUpdated {
                session: session.clone(),
            });
        }
        for _ in 0..3 {
            registry.emit(RegistryEvent::Output {
                id: id.clone(),
                data: b"output".to_vec(),
            });
        }

        assert_eq!(events.try_iter().count(), SUBSCRIBER_QUEUE_CAPACITY);
        assert_eq!(registry.admission_snapshot().dropped_events, 3);
    }

    #[test]
    fn admission_queues_overflow_and_applies_default_budget() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(1, Some(4.5)));
        let cwd = Path::new(".").to_path_buf();
        let active_id = SessionId::new(99);
        let spec = spec_for(Agent::Claude, &cwd);
        let mut active = Session::new(active_id.clone(), &spec);
        active.status = SessionStatus::Idle;
        active.phase = SessionPhase::Idle;
        active.health = crate::session::SessionHealth::Degraded;
        lock_state(&registry.inner).entries.insert(
            active_id.clone(),
            Entry {
                session: active,
                spec: spec.clone(),
                command: ResolvedCommand {
                    program: PathBuf::from("active-agent"),
                    args: Vec::new(),
                    cwd: cwd.clone(),
                },
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );

        let queued_id = registry
            .launch(
                spec,
                AgentBinary {
                    agent: Agent::Claude,
                    path: PathBuf::from("missing-terminalai-test-agent.exe"),
                    origin: Origin::Path,
                },
            )
            .expect("overflow is queued before spawn");
        assert!(registry.is_queued(&queued_id).expect("queued row"));
        let snapshot = registry.admission_snapshot();
        assert_eq!(snapshot.max_live_sessions, 1);
        assert_eq!(snapshot.live_sessions, 1);
        assert_eq!(snapshot.queued_sessions, 1);
        assert_eq!(
            lock_state(&registry.inner).entries[&queued_id]
                .spec
                .max_budget_usd,
            Some(4.5)
        );

        lock_state(&registry.inner).entries.remove(&active_id);
        registry.drain_queue();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline
            && registry.snapshot()[0].status != SessionStatus::Exited
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            registry.snapshot()[0].status,
            SessionStatus::Exited,
            "a failed queued spawn remains visible as stopped"
        );
        assert_eq!(registry.admission_snapshot().queued_sessions, 0);
    }

    #[test]
    fn failed_process_query_marks_unknown_without_deleting_state() {
        let registry = SessionRegistry::new();
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let id = SessionId::new(1);
        let mut session = Session::new(id.clone(), &spec);
        session.status = SessionStatus::Working;
        session.phase = SessionPhase::Working;
        lock_state(&registry.inner).entries.insert(
            id.clone(),
            Entry {
                session,
                spec,
                command: ResolvedCommand {
                    program: PathBuf::from("test-agent"),
                    args: Vec::new(),
                    cwd,
                },
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );

        let events = registry.subscribe();
        registry.mark_process_unknown(&id, 1);
        let row = registry.snapshot().pop().expect("row survives");
        assert_eq!(row.status, SessionStatus::Unknown);
        assert_eq!(row.phase, SessionPhase::Unknown);
        assert_eq!(registry.store_snapshot().sessions.len(), 1);
        assert!(events.try_iter().any(|event| matches!(
            event,
            RegistryEvent::SessionUpdated { session } if session.id == id
        )));
        assert!(!events
            .try_iter()
            .any(|event| matches!(event, RegistryEvent::SessionRemoved { .. })));
    }

    #[test]
    fn output_updates_the_background_grid_alongside_scrollback() {
        let registry = SessionRegistry::new();
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let id = SessionId::new(1);
        let session = Session::new(id.clone(), &spec);
        lock_state(&registry.inner).entries.insert(
            id.clone(),
            Entry {
                session,
                command: ResolvedCommand {
                    program: PathBuf::from("test-agent"),
                    args: Vec::new(),
                    cwd: cwd.clone(),
                },
                spec,
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );

        let events = registry.subscribe();
        handle_output(&registry.inner, &id, 1, b"hello\x1b[2;1Hworld");
        assert!(events
            .try_iter()
            .all(|event| !matches!(event, RegistryEvent::Output { .. })));
        assert_eq!(
            registry.scrollback(&id).expect("scrollback"),
            b"hello\x1b[2;1Hworld"
        );
        assert_eq!(registry.grid_snapshot(&id).expect("grid").lines[1], "world");
        registry.focus(Some(id.clone())).expect("focus");
        let _ = events.try_iter().count();
        handle_output(&registry.inner, &id, 1, &[0xf0, 0x9f]);
        handle_output(&registry.inner, &id, 1, &[0x98, 0x80]);
        let output = events
            .try_iter()
            .filter_map(|event| match event {
                RegistryEvent::Output { data, .. } => Some(data),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(output, "😀".as_bytes());
    }

    #[test]
    fn store_restore_keeps_rows_and_replay_bytes_without_starting_processes() {
        let cwd = Path::new(".").to_path_buf();
        let mut spec = spec_for(Agent::Claude, &cwd);
        spec.resume = Resume::Session("native-1".into());
        let mut session = Session::new(SessionId::new(7), &spec);
        session.resume_id = Some("native-1".into());
        let snapshot = SessionStoreSnapshot {
            schema_version: crate::store::SESSION_STORE_SCHEMA_VERSION,
            sessions: vec![StoredSession {
                session,
                spec,
                command: ResolvedCommand {
                    program: PathBuf::from("claude.exe"),
                    args: vec!["--resume".into(), "native-1".into()],
                    cwd,
                },
                scrollback: b"restored\r\n".to_vec(),
            }],
            archives: Vec::new(),
        };

        let registry = SessionRegistry::from_store(snapshot);
        let session = &registry.snapshot()[0];
        assert_eq!(session.id, SessionId::new(7));
        assert_eq!(session.phase, SessionPhase::Resurrectable);
        assert_eq!(session.status, SessionStatus::Exited);
        assert!(session.pid.is_none());
        assert_eq!(
            registry.scrollback(&SessionId::new(7)).expect("scrollback"),
            b"restored\r\n"
        );
        assert_eq!(
            registry
                .grid_snapshot(&SessionId::new(7))
                .expect("grid")
                .lines[0],
            "restored"
        );
    }

    #[test]
    fn archive_removes_only_stopped_rows_and_records_the_layout() {
        let registry = SessionRegistry::new();
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let id = SessionId::new(1);
        let mut session = Session::new(id.clone(), &spec);
        session.status = SessionStatus::NeedsApproval;
        session.state_since = std::time::SystemTime::UNIX_EPOCH;
        {
            let mut state = lock_state(&registry.inner);
            state.entries.insert(
                id.clone(),
                Entry {
                    session: session.clone(),
                    command: ResolvedCommand {
                        program: PathBuf::from("claude.exe"),
                        args: vec!["--resume".into(), "native-1".into()],
                        cwd: cwd.clone(),
                    },
                    spec,
                    pty: None,
                    scrollback: RingBuffer::default(),
                    grid: TerminalGrid::default(),
                    generation: 1,
                    stop_requested: false,
                    branch_checked: None,
                    teardown_done: true,
                    span: tracing::Span::none(),
                },
            );
            state.notifications.observe(
                &session,
                SessionStatus::Idle,
                std::time::SystemTime::UNIX_EPOCH,
                std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(100),
            );
        }
        let events = registry.subscribe();

        let archive = registry.archive(&id).expect("archive");
        assert_eq!(archive.id, id);
        assert!(events.try_iter().any(|event| matches!(
            event,
            RegistryEvent::Notification {
                event: NotificationEvent::Retracted { session_id, .. }
            } if session_id == id
        )));
        assert!(registry.snapshot().is_empty());
        let stored = registry.store_snapshot();
        assert!(stored.sessions.is_empty());
        assert_eq!(stored.archives.len(), 1);
        assert_eq!(stored.archives[0].command, "claude.exe --resume native-1");
    }

    #[test]
    fn killing_queued_rows_retracts_attention_notifications() {
        let registry = SessionRegistry::new();
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let id = SessionId::new(1);
        let mut session = Session::new(id.clone(), &spec);
        session.set_status(SessionStatus::Queued);
        let mut attention = session.clone();
        attention.status = SessionStatus::NeedsApproval;
        attention.state_since = std::time::SystemTime::UNIX_EPOCH;
        {
            let mut state = lock_state(&registry.inner);
            state.entries.insert(
                id.clone(),
                Entry {
                    session,
                    command: ResolvedCommand {
                        program: PathBuf::from("test-agent.exe"),
                        args: Vec::new(),
                        cwd: cwd.clone(),
                    },
                    spec,
                    pty: None,
                    scrollback: RingBuffer::default(),
                    grid: TerminalGrid::default(),
                    generation: 1,
                    stop_requested: false,
                    branch_checked: None,
                    teardown_done: true,
                    span: tracing::Span::none(),
                },
            );
            state.queue.push_back(id.clone());
            state.notifications.observe(
                &attention,
                SessionStatus::Idle,
                std::time::SystemTime::UNIX_EPOCH,
                std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(100),
            );
        }
        let events = registry.subscribe();

        registry.kill(&id).expect("kill queued row");

        assert!(events.try_iter().any(|event| matches!(
            event,
            RegistryEvent::Notification {
                event: NotificationEvent::Retracted { session_id, .. }
            } if session_id == id
        )));
        let state = lock_state(&registry.inner);
        assert!(state.notifications.active().is_empty());
    }

    #[test]
    fn snapshots_are_sorted_by_attention() {
        let registry = SessionRegistry::new();
        let mut state = lock_state(&registry.inner);
        let cwd = Path::new(".").to_path_buf();
        for (id, status) in [
            ("s0001", SessionStatus::Idle),
            ("s0002", SessionStatus::NeedsYou),
        ] {
            let spec = spec_for(Agent::Claude, &cwd);
            let mut session = Session::new(SessionId(id.into()), &spec);
            session.status = status;
            state.entries.insert(
                session.id.clone(),
                Entry {
                    session,
                    command: ResolvedCommand {
                        program: PathBuf::from("test-agent"),
                        args: Vec::new(),
                        cwd: cwd.clone(),
                    },
                    spec,
                    pty: None,
                    scrollback: RingBuffer::default(),
                    grid: TerminalGrid::default(),
                    generation: 1,
                    stop_requested: false,
                    branch_checked: None,
                    teardown_done: true,
                    span: tracing::Span::none(),
                },
            );
        }
        drop(state);
        assert_eq!(registry.snapshot()[0].status, SessionStatus::NeedsYou);
    }

    #[test]
    fn mismatched_binaries_are_refused_before_spawn() {
        let registry = SessionRegistry::new();
        let spec = spec_for(Agent::Claude, Path::new("."));
        let binary = AgentBinary {
            agent: Agent::Codex,
            path: PathBuf::from("codex.exe"),
            origin: Origin::Configured,
        };
        assert!(matches!(
            registry.launch(spec, binary),
            Err(RegistryError::AgentMismatch { .. })
        ));
    }

    #[test]
    fn hooks_bind_resume_id_and_distinguish_attention_states() {
        let registry = SessionRegistry::new();
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let id = SessionId::new(1);
        let session = Session::new(id.clone(), &spec);
        lock_state(&registry.inner).entries.insert(
            id.clone(),
            Entry {
                session,
                command: ResolvedCommand {
                    program: PathBuf::from("test-agent"),
                    args: Vec::new(),
                    cwd: cwd.clone(),
                },
                spec,
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );

        assert!(registry.apply_hook(HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd.clone()),
            signal: HookSignal::SessionStart,
            progress: None,
        }));
        assert!(registry.apply_hook(HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd.clone()),
            signal: HookSignal::Notification {
                notification: HookNotification::PermissionPrompt,
            },
            progress: None,
        }));
        let session = registry.snapshot().pop().expect("session");
        assert_eq!(session.resume_id.as_deref(), Some("native-1"));
        assert_eq!(session.status, SessionStatus::NeedsApproval);
        assert!(session.unread);

        assert!(registry.apply_hook(HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd),
            signal: HookSignal::Stop,
            progress: None,
        }));
        assert_eq!(registry.snapshot()[0].status, SessionStatus::Idle);
    }

    #[test]
    fn attention_notifications_dedupe_and_retract_on_progress() {
        let registry = SessionRegistry::new();
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let id = SessionId::new(1);
        let mut session = Session::new(id.clone(), &spec);
        session.status = SessionStatus::Idle;
        session.state_since = std::time::SystemTime::UNIX_EPOCH;
        lock_state(&registry.inner).entries.insert(
            id.clone(),
            Entry {
                session,
                command: ResolvedCommand {
                    program: PathBuf::from("test-agent"),
                    args: Vec::new(),
                    cwd: cwd.clone(),
                },
                spec,
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );
        let events = registry.subscribe();
        let attention = HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd.clone()),
            signal: HookSignal::Notification {
                notification: HookNotification::PermissionPrompt,
            },
            progress: None,
        };

        assert!(registry.apply_hook(attention.clone()));
        let first: Vec<_> = events.try_iter().collect();
        assert!(first.iter().any(|event| matches!(
            event,
            RegistryEvent::Notification {
                event: NotificationEvent::Raised { notification }
            } if notification.dedup_key.contains("session=s0001")
        )));

        assert!(registry.apply_hook(attention));
        assert!(!events
            .try_iter()
            .any(|event| matches!(event, RegistryEvent::Notification { .. })));

        assert!(registry.apply_hook(HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd),
            signal: HookSignal::PreToolUse,
            progress: None,
        }));
        assert!(events.try_iter().any(|event| matches!(
            event,
            RegistryEvent::Notification {
                event: NotificationEvent::Retracted { .. }
            }
        )));
    }

    #[test]
    fn hooks_from_other_sessions_are_ignored() {
        let registry = SessionRegistry::new();
        assert!(!registry.apply_hook(HookEvent {
            agent: Agent::Codex,
            session_id: Some("missing".into()),
            cwd: Some(PathBuf::from(".")),
            signal: HookSignal::Stop,
            progress: None,
        }));
    }

    #[test]
    fn app_server_events_update_codex_rows_and_remain_on_the_event_stream() {
        let registry = SessionRegistry::new();
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Codex, &cwd);
        let id = SessionId::new(1);
        let mut session = Session::new(id.clone(), &spec);
        session.resume_id = Some("thread-1".into());
        lock_state(&registry.inner).entries.insert(
            id.clone(),
            Entry {
                session,
                command: ResolvedCommand {
                    program: PathBuf::from("test-agent"),
                    args: Vec::new(),
                    cwd: cwd.clone(),
                },
                spec,
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );
        let events = registry.subscribe();

        assert!(registry.apply_agent_event(AgentEvent::AppServer(
            AppServerEvent::ThreadStatusChanged {
                thread_id: "thread-1".into(),
                status: AppServerThreadStatus {
                    kind: "active".into(),
                    active_flags: vec!["waitingOnApproval".into()],
                },
            },
        )));
        assert_eq!(registry.snapshot()[0].status, SessionStatus::NeedsApproval);

        assert!(registry.apply_agent_event(AgentEvent::AppServer(
            AppServerEvent::TokenUsageUpdated {
                thread_id: "thread-1".into(),
                usage: AppServerTokenUsage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 3,
                    model_context_window: None,
                },
            },
        )));
        assert!(events.try_iter().any(|event| matches!(
            event,
            RegistryEvent::AgentEvent {
                event: AgentEvent::AppServer(AppServerEvent::TokenUsageUpdated { .. })
            }
        )));
    }

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
        assert!(
            started.elapsed() < Duration::from_secs(1),
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
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline && !marker.exists() {
            std::thread::sleep(Duration::from_millis(25));
        }
        let teardown = std::fs::read_to_string(&marker).expect("teardown marker");
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
