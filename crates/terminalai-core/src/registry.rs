//! Rust-owned session lifetime and fleet state.
//!
//! The GUI is intentionally not the owner of a process. A [`SessionRegistry`]
//! keeps the launch specification, live pty, bounded scrollback, focus and
//! fleet metadata together, then publishes small events to any interested
//! shell. This makes closing or reloading a view harmless to live agents.

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::admission::FleetDemand;
use crate::agent::{AgentBinary, Origin};
use crate::app_server::{AgentEvent, AppServerEvent};
use crate::diagnostics::{LogEntry, StatusSource};
use crate::lease;
use crate::domain::{AgentDomain, AgentSession, DomainError, LocalPtyDomain};
use crate::environment::{self, EnvironmentError, EnvironmentSpec};
use crate::grid::{TerminalGrid, TerminalGridSnapshot};
use crate::hooks::{HookEvent, HookNotification, HookSignal};
use crate::launch::{is_valid_resume_id, LaunchError, LaunchSpec, ResolvedCommand, Resume};
use crate::notification::{NotificationCenter, NotificationChange, NotificationEvent};
use crate::pty::{PtySize, StopOutcome};
use crate::review::{collect_reviews, ReviewItem};
use crate::scrollback::ScrollbackSpool;
use crate::session::{
    fleet_order, fresh_hook_token, fresh_native_session_id, RateLimit, RestartDecision, Session,
    SessionId, SessionPhase, SessionStatus,
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
const NOTIFICATION_RECHECK_INTERVAL: Duration = Duration::from_millis(250);

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

/// The admission gate itself lives in [`crate::admission`]: it decides over a
/// summary passed in rather than over this module's lock and clock. Re-exported
/// here because every caller reaches admission through the registry.
pub use crate::admission::{
    assumed_session_bytes, AdmissionBlock, AdmissionConfig, AdmissionSnapshot,
    ASSUMED_SESSION_BYTES_CLAUDE, ASSUMED_SESSION_BYTES_CODEX, DEFAULT_MAX_LIVE_SESSIONS,
    DEFAULT_SESSION_BUDGET_USD,
};

/// Events are deliberately coarse: a view can rebuild its rows from a session
/// update and only the focused pane needs to consume output bytes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub enum RegistryEvent {
    // Boxed: `Session` is by far the largest payload here and every slot in a
    // bounded subscriber queue is sized for the biggest variant. The extra
    // allocation is nothing beside the JSON serialization each event already
    // pays to cross the pipe.
    SessionUpdated { session: Box<Session> },
    Notification { event: NotificationEvent },
    AgentEvent { event: AgentEvent },
    Log { entry: LogEntry },
    Output { id: SessionId, data: Vec<u8> },
    SessionRemoved { id: SessionId },
}

/// What a session needs from its spec at spawn time: working directory, the
/// hook and port spec, the reserved ports, the per-session hook secret, and the
/// variables the launch itself asked for.
type RuntimeEnvironment = (
    std::path::PathBuf,
    EnvironmentSpec,
    Vec<u16>,
    String,
    Vec<(String, String)>,
);

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    #[error(transparent)]
    Domain(#[from] DomainError),
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
    #[error("{0}")]
    Queue(#[from] crate::queue::QueueError),
}

struct Entry {
    session: Session,
    spec: LaunchSpec,
    command: ResolvedCommand,
    pty: Option<Arc<dyn AgentSession>>,
    scrollback: RingBuffer,
    grid: TerminalGrid,
    /// Prompts waiting their turn on this session.
    queue: crate::queue::PromptQueue,
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
    /// Session ids whose focused terminal has received input that has not yet
    /// been explicitly submitted or superseded by agent activity.
    operator_edited: BTreeSet<SessionId>,
    entries: BTreeMap<SessionId, Entry>,
    archives: Vec<ArchivedSession>,
    extra: BTreeMap<String, serde_json::Value>,
    queue: VecDeque<SessionId>,
    admission: AdmissionConfig,
    /// Fleet spend over the rolling window the ceiling is measured against.
    spend: crate::spend::SpendLedger,
    /// What each agent last said about its credentials.
    auth: BTreeMap<crate::agent::Agent, crate::auth::AgentAuth>,
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
    /// Transcript readers have their own lock because polling performs file
    /// reads and directory walks. The registry state lock must remain free so
    /// PTY output and hook ingestion cannot be back-pressured by a slow disk.
    tails: Mutex<crate::tail::TranscriptTails>,
    domain: Arc<dyn AgentDomain>,
    dropped_events: AtomicU64,
    restart_tx: Sender<RestartTask>,
    restart_sequence: AtomicU64,
    /// The disk tier under the in-memory ring, when one is configured. Absent
    /// in tests and in the in-process app-server, where a session's history
    /// dies with the process that owns it anyway.
    spool: Mutex<Option<Arc<ScrollbackSpool>>>,
    /// Where per-session Git worktrees are cut. Absent means isolation was
    /// never configured, so a session that requests it is refused.
    worktree_root: Mutex<Option<std::path::PathBuf>>,
}

impl Inner {
    fn spool(&self) -> Option<Arc<ScrollbackSpool>> {
        self.spool.lock().ok().and_then(|spool| spool.clone())
    }
}

impl Inner {
    fn is_poisoned(&self) -> bool {
        self.state.is_poisoned()
    }
}

/// Why one session did not receive a broadcast.
///
/// Named cases rather than a message, so the UI can tell "this one is not
/// running" from "this one is waiting for a permission decision" — the operator
/// does something different about each.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BroadcastRefusal {
    /// No such session.
    Missing,
    /// The row exists but has no live process behind it.
    NotRunning,
    /// Waiting on a permission decision, where prompt text is not an answer.
    NeedsApproval,
    /// The operator is composing in the focused pane.
    FocusedAndEdited,
    /// The write itself failed.
    WriteFailed(String),
}

impl std::fmt::Display for BroadcastRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(formatter, "no such session"),
            Self::NotRunning => write!(formatter, "not running"),
            Self::NeedsApproval => {
                write!(formatter, "waiting for a permission decision; answer it directly")
            }
            Self::FocusedAndEdited => write!(
                formatter,
                "focused and edited; defocus it or send explicitly"
            ),
            Self::WriteFailed(detail) => write!(formatter, "write failed: {detail}"),
        }
    }
}

/// What happened to one session in a broadcast.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BroadcastResult {
    pub id: SessionId,
    /// `None` means the bytes were written.
    pub refusal: Option<BroadcastRefusal>,
}

impl BroadcastResult {
    pub fn delivered(&self) -> bool {
        self.refusal.is_none()
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
        Self::with_domain_and_admission(Arc::new(LocalPtyDomain), admission)
    }

    /// Use an injected execution domain with the default admission policy.
    /// Remote domains can implement the same session contract without
    /// exposing a local process handle to the registry.
    pub fn with_domain(domain: Arc<dyn AgentDomain>) -> Self {
        Self::with_domain_and_admission(domain, AdmissionConfig::default())
    }

    pub fn with_domain_and_admission(
        domain: Arc<dyn AgentDomain>,
        admission: AdmissionConfig,
    ) -> Self {
        let (restart_tx, restart_rx) = mpsc::channel();
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                next_id: 1,
                focused: None,
                operator_edited: BTreeSet::new(),
                entries: BTreeMap::new(),
                archives: Vec::new(),
                extra: BTreeMap::new(),
                queue: VecDeque::new(),
                admission,
                spend: crate::spend::SpendLedger::new(),
                auth: BTreeMap::new(),
                notifications: NotificationCenter::default(),
                subscribers: Vec::new(),
            }),
            tails: Mutex::new(crate::tail::TranscriptTails::default()),
            domain,
            spool: Mutex::new(None),
            worktree_root: Mutex::new(None),
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

    pub fn from_store_with_domain(
        snapshot: SessionStoreSnapshot,
        domain: Arc<dyn AgentDomain>,
    ) -> Self {
        Self::from_store_with_domain_and_admission(snapshot, domain, AdmissionConfig::default())
    }

    pub fn from_store_with_admission(
        snapshot: SessionStoreSnapshot,
        admission: AdmissionConfig,
    ) -> Self {
        Self::from_store_with_domain_and_admission(snapshot, Arc::new(LocalPtyDomain), admission)
    }

    pub fn from_store_with_domain_and_admission(
        snapshot: SessionStoreSnapshot,
        domain: Arc<dyn AgentDomain>,
        admission: AdmissionConfig,
    ) -> Self {
        let registry = Self::with_domain_and_admission(domain, admission);
        let mut state = lock_state(&registry.inner);
        let extra = snapshot.extra;
        let mut archives = snapshot.archives;
        // `next_id` is computed over every archive *before* trimming: an id that
        // has been handed out must never be handed out again, even once the
        // record naming it has aged out.
        for archive in &archives {
            state.next_id = state.next_id.max(next_sequence(&archive.id));
        }
        // A store written before the bound existed, or by a build without it,
        // is brought inside the limit on the way in rather than left oversized
        // until the next archive happens to trim it.
        crate::store::trim_archives(&mut archives, SystemTime::now());
        state.archives = archives;
        state.extra = extra;
        // Spend that already happened still counts against the window, so a
        // restart cannot be used to clear the ceiling. Anything older than the
        // window is dropped on the way in rather than carried as dead weight.
        state.spend = crate::spend::SpendLedger::from_buckets(snapshot.spend);
        let window = state.admission.spend_window;
        state.spend.prune_at(SystemTime::now(), window);
        for stored in snapshot.sessions {
            let StoredSession {
                mut session,
                spec,
                command,
                scrollback: bytes,
                mut queue,
            } = stored;
            let id = session.id.clone();
            if session.hook_token.is_empty() {
                session.hook_token = fresh_hook_token();
            }
            if session.ports.is_empty() && spec.environment.port_count > 0 {
                session.ports = spec
                    .environment
                    .ports_for_session(&id.0)
                    .unwrap_or_default();
            }
            let exit_code = session.last_exit_code;
            session.mark_resurrectable_at_from(exit_code, SystemTime::now(), StatusSource::Restore);
            // A restored session is not running, so its queue must not start
            // firing at whatever status the restore left behind.
            queue.pause(crate::queue::PauseReason::NotRunning);
            session.queued_prompts = queue.len();
            session.queue_paused = queue.paused();
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
                    queue,
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

    /// Record what an agent reported about its credentials.
    ///
    /// Emitted as a session-independent fact: the fleet needs one banner, not
    /// one failure per queued entry.
    pub fn set_agent_auth(&self, auth: crate::auth::AgentAuth) -> bool {
        let mut state = lock_state(&self.inner);
        let changed = state.auth.get(&auth.agent) != Some(&auth);
        state.auth.insert(auth.agent, auth);
        changed
    }

    pub fn agent_auth(&self, agent: crate::agent::Agent) -> Option<crate::auth::AgentAuth> {
        lock_state(&self.inner).auth.get(&agent).cloned()
    }

    /// Whether work for this agent should hold rather than start.
    ///
    /// Only an explicit expiry holds. `Unknown` deliberately does not: a probe
    /// that could not run must never be able to stop the fleet.
    pub fn auth_holds(&self, agent: crate::agent::Agent) -> bool {
        lock_state(&self.inner)
            .auth
            .get(&agent)
            .is_some_and(|auth| auth.state == crate::auth::AuthState::Expired)
    }

    /// Replace the daemon-wide admission policy without a restart.
    ///
    /// Applies to every later decision, including automatic restarts, because
    /// the registry is where the gate reads from. Sessions already running are
    /// untouched: a limit is an admission policy, not a kill switch.
    pub fn set_admission(&self, admission: AdmissionConfig) {
        {
            let mut state = lock_state(&self.inner);
            let window = admission.spend_window;
            state.admission = admission;
            // The window may have shrunk; drop what no longer counts so the
            // reported figure matches the policy that is now in force.
            state.spend.prune_at(SystemTime::now(), window);
        }
        // A raised cap can admit immediately; a lowered one simply stops
        // granting. Either way the queue is re-evaluated rather than waiting
        // for the next unrelated event.
        self.drain_queue();
    }

    pub fn admission_config(&self) -> AdmissionConfig {
        lock_state(&self.inner).admission
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
            spend_window_usd: state
                .spend
                .window_total_at(SystemTime::now(), state.admission.spend_window),
            spend_ceiling_usd: state.admission.spend_ceiling_usd,
            spend_window_hours: state.admission.spend_window.as_secs_f64() / 3600.0,
            admission_block: admission_block(&state),
            memory_budget_bytes: state.admission.memory_budget_bytes,
            projected_memory_bytes: projected_memory_bytes(&state),
            session_memory_cap_bytes: state.admission.session_memory_cap_bytes,
            memory_limited_sessions: state
                .entries
                .values()
                .filter(|entry| entry.session.memory_limited)
                .count(),
            expired_auth: state
                .auth
                .values()
                .filter(|auth| auth.state == crate::auth::AuthState::Expired)
                .cloned()
                .collect(),
            // Claude takes `--max-budget-usd`; Codex documents no equivalent, so
            // saying "budget" without naming which agent it binds would claim an
            // enforcement that does not exist for half the fleet.
            budget_enforced_agents: vec![crate::agent::Agent::Claude.command_name().to_string()],
            rate_limited_sessions: state
                .entries
                .values()
                .filter(|entry| entry.session.status == SessionStatus::RateLimited)
                .count(),
            earliest_rate_limit_reset: state
                .entries
                .values()
                .filter(|entry| entry.session.status == SessionStatus::RateLimited)
                .filter_map(|entry| entry.session.rate_limit.as_ref()?.resets_at)
                .min(),
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
        // With a disk tier attached the log is the durable copy of a session's
        // output, so the store carries none. It is rewritten in full on a
        // debounce — copying every session's whole ring into it once a second
        // was the most expensive thing persistence did, and it duplicated bytes
        // the spool had already appended.
        let store_scrollback = self.inner.spool().is_none();
        let state = lock_state(&self.inner);
        SessionStoreSnapshot {
            magic: crate::store::SESSION_STORE_MAGIC.to_owned(),
            schema_version: crate::store::SESSION_STORE_SCHEMA_VERSION,
            spend: state.spend.buckets().copied().collect(),
            sessions: state
                .entries
                .values()
                .map(|entry| StoredSession {
                    session: entry.session.clone(),
                    spec: entry.spec.clone(),
                    command: entry.command.clone(),
                    queue: entry.queue.clone(),
                    scrollback: if store_scrollback {
                        entry.scrollback.to_vec()
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
            archives: state.archives.clone(),
            extra: state.extra.clone(),
        }
    }

    /// Finished sessions, newest first.
    ///
    /// The store has always carried these — id, agent, name, folder and the
    /// exact command — and read them back only to advance the id counter. They
    /// are bounded by [`crate::store::MAX_ARCHIVES`] and
    /// [`crate::store::ARCHIVE_MAX_AGE`], so this is a whole answer rather than
    /// a page of one.
    pub fn archives(&self) -> Vec<ArchivedSession> {
        let state = lock_state(&self.inner);
        state.archives.iter().rev().cloned().collect()
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
        if spec.agent == crate::agent::Agent::Claude
            && spec.session_id.is_none()
            && matches!(spec.resume, Resume::New)
        {
            spec.session_id = fresh_native_session_id();
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
            let queued = admission_block(&state).is_some();
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
                    queue: crate::queue::PromptQueue::default(),
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

    /// Write bytes originating in the focused terminal. A raw terminal stream
    /// has no separate "editing" event, so a line ending is the explicit-send
    /// boundary and every other keystroke keeps automatic delivery held.
    /// Programmatic queue and broadcast writes use [`Self::write`] and never
    /// clear this guard accidentally.
    pub fn write_user_input(
        &self,
        id: &SessionId,
        bytes: &[u8],
    ) -> Result<(), RegistryError> {
        let pty = self.pty(id)?;
        let explicit_send = bytes.iter().any(|byte| matches!(byte, b'\r' | b'\n'));
        let changed = {
            let mut state = lock_state(&self.inner);
            let focused = state.focused.as_ref() == Some(id);
            let should_hold = focused && !explicit_send;
            let was_edited = state.operator_edited.contains(id);
            if should_hold {
                state.operator_edited.insert(id.clone());
            } else {
                state.operator_edited.remove(id);
            }
            let entry = state
                .entries
                .get_mut(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?;
            let before_pause = entry.queue.paused();
            if should_hold {
                entry.queue.hold_for_focus_edit();
            } else {
                entry.queue.clear_focus_edit();
            }
            entry.session.queued_prompts = entry.queue.len();
            entry.session.queue_paused = entry.queue.paused();
            was_edited != should_hold || before_pause != entry.queue.paused()
        };
        if changed {
            self.emit_session(id);
        }
        pty.write(bytes).map_err(RegistryError::from)
    }

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
    fn pump_queue(&self, id: &SessionId) {
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

    /// Focus a live process and return the bounded raw tail for renderer
    /// replay. The GUI uses this when it reconnects to the already-running
    /// daemon after a window reload.
    pub fn reattach(&self, id: &SessionId) -> Result<Vec<u8>, RegistryError> {
        let pty = self.pty(id)?;
        if pty.try_wait()?.is_some() {
            return Err(RegistryError::NotRunning(id.clone()));
        }
        self.focus(Some(id.clone()))?;
        pty.set_renderer_attached(true);
        self.scrollback(id)
    }

    /// Explicitly revive a non-running row through its native resume command.
    /// Automatic restart never calls this path.
    pub fn revive(&self, id: &SessionId) -> Result<SessionId, RegistryError> {
        let (command, generation, queued) = {
            let mut state = lock_state(&self.inner);
            let admission_full = admission_block(&state).is_some();
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
    /// Record that this session's work landed.
    ///
    /// Kept on the row rather than only in the landing's response, because the
    /// response is read once and thrown away while the question it answers —
    /// did this session finish, or did someone abandon it — is asked every time
    /// anyone looks at the leftover checkouts.
    pub fn record_landing(
        &self,
        id: &SessionId,
        landing: crate::land::Landing,
    ) -> Result<(), RegistryError> {
        let session = {
            let mut state = lock_state(&self.inner);
            let entry = state
                .entries
                .get_mut(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?;
            entry.session.landed = Some(landing);
            entry.session.clone()
        };
        self.emit(RegistryEvent::SessionUpdated {
            session: Box::new(session),
        });
        Ok(())
    }

    pub fn archive(&self, id: &SessionId) -> Result<ArchivedSession, RegistryError> {
        {
            let state = lock_state(&self.inner);
            match state.entries.get(id) {
                None => return Err(RegistryError::Missing(id.clone())),
                Some(entry) if entry.pty.is_some() => {
                    return Err(RegistryError::StillRunning(id.clone()))
                }
                Some(_) => {}
            }
        }
        // Before the entry is dropped, while its worktree is still recorded.
        // A branch holding unmerged work is kept and reported, never deleted.
        let worktree_failures = self.release_worktree(id);
        if !worktree_failures.is_empty() {
            tracing::warn!(session = %id, ?worktree_failures, "session worktree was not fully removed");
        }
        let (archived, notifications) = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get(id) else {
                return Err(RegistryError::Missing(id.clone()));
            };
            if entry.pty.is_some() {
                return Err(RegistryError::StillRunning(id.clone()));
            }
            let entry = state.entries.remove(id).expect("entry checked above");
            state.operator_edited.remove(id);
            state.queue.retain(|queued| queued != id);
            if state.focused.as_ref() == Some(id) {
                state.focused = None;
            }
            let now = SystemTime::now();
            let archived = ArchivedSession::from_session_at(&entry.session, &entry.command, now);
            state.archives.retain(|item| item.id != *id);
            state.archives.push(archived.clone());
            // Bounded here, where the list grows, so the snapshot writer never
            // has to serialize an archive list that outgrew its limit.
            crate::store::trim_archives(&mut state.archives, now);
            let notifications = state.notifications.retract_session(id);
            (archived, notifications)
        };
        // An archive keeps the layout and the command, never the output. The
        // history is what the disk is being spent on, so it goes with the row.
        spool_forget(&self.inner, id);
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
                    self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
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
                    self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
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
                    self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
                    self.emit_notification_changes(notifications);
                    self.drain_queue();
                    return Ok(());
                }
                return Err(RegistryError::Missing(id.clone()));
            };
            // The stop is no longer instantaneous, so say so: the row shows the
            // agent shutting down instead of looking untouched for five seconds.
            // The exit path overwrites this phase when the process actually goes.
            entry.session.mark_tearing_down();
            let generation = entry.generation;
            let session = entry.session.clone();
            drop(state);
            self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
            (pty, generation)
        };

        // The stop ladder is bounded but not instant, and this runs on the
        // client connection's dispatch thread — blocking it would freeze every
        // other request on that connection, snapshots included, for the whole
        // grace period. Hand the ladder to a worker and let the session's own
        // exit monitor report the result, which it does with the real exit code
        // rather than the `None` this path used to invent.
        let registry = self.clone();
        let worker_id = id.clone();
        let worker_pty = pty.clone();
        let spawned = thread::Builder::new()
            .name(format!("terminalai-stop-{id}"))
            .spawn(move || match worker_pty.stop() {
                Ok(StopOutcome::Graceful) => {}
                Ok(StopOutcome::Terminated) => {
                    tracing::warn!(
                        session = %worker_id,
                        "agent did not shut down within its grace period; the job was terminated"
                    );
                    registry.mark_process_exit(&worker_id, generation, None);
                }
                Err(error) => {
                    tracing::error!(session = %worker_id, %error, "could not stop the agent");
                    registry.mark_process_exit(&worker_id, generation, None);
                }
            });
        if let Err(error) = spawned {
            // Thread exhaustion. A session the operator asked to stop must stop,
            // so fall back to the immediate kill on this thread rather than
            // leaving it running.
            tracing::error!(
                session = %id,
                %error,
                "could not start a stop worker; terminating the agent immediately"
            );
            if let Err(error) = pty.kill() {
                let mut state = lock_state(&self.inner);
                if let Some(entry) = state.entries.get_mut(id) {
                    entry.stop_requested = false;
                }
                return Err(error.into());
            }
            self.mark_process_exit(id, generation, None);
        }
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

    pub fn focus(&self, id: Option<SessionId>) -> Result<(), RegistryError> {
        if let Some(id) = &id {
            self.require(id)?;
        }
        let (renderer_to_detach, priorities, previous_to_resume) = {
            let mut state = lock_state(&self.inner);
            let previous = state.focused.clone();
            state.focused = id.clone();
            let previous_to_resume = if previous.as_ref() != id.as_ref() {
                if let Some(previous_id) = previous.as_ref() {
                    state.operator_edited.remove(previous_id);
                    if let Some(entry) = state.entries.get_mut(previous_id) {
                        entry.queue.clear_focus_edit();
                        entry.session.queued_prompts = entry.queue.len();
                        entry.session.queue_paused = entry.queue.paused();
                    }
                }
                previous.clone()
            } else {
                None
            };
            let renderer_to_detach = if previous.as_ref() != id.as_ref() {
                previous.as_ref().and_then(|previous| {
                    state
                        .entries
                        .get(previous)
                        .and_then(|entry| entry.pty.clone())
                })
            } else {
                None
            };
            if let Some(id) = &id {
                if let Some(entry) = state.entries.get_mut(id) {
                    entry.session.unread = false;
                }
            }
            let mut candidates = Vec::new();
            if let Some(previous) = previous {
                candidates.push(previous);
            }
            if let Some(id) = id.clone() {
                if !candidates.contains(&id) {
                    candidates.push(id);
                }
            }
            let priorities = candidates
                .into_iter()
                .filter_map(|candidate| {
                    let entry = state.entries.get(&candidate)?;
                    let pty = entry.pty.clone()?;
                    let background =
                        state.focused.as_ref() != Some(&candidate) && !entry.session.pinned;
                    Some((pty, background))
                })
                .collect::<Vec<_>>();
            (renderer_to_detach, priorities, previous_to_resume)
        };
        if let Some(pty) = renderer_to_detach {
            pty.set_renderer_attached(false);
        }
        for (pty, background) in priorities {
            if let Err(error) = pty.set_background(background) {
                tracing::warn!(
                    background,
                    error = %error,
                    "could not update process priority after focus change"
                );
            }
        }
        if let Some(previous) = previous_to_resume {
            self.emit_session(&previous);
            self.pump_queue(&previous);
        }
        if let Some(id) = id {
            self.emit_session(&id);
        }
        Ok(())
    }

    pub fn toggle_pin(&self, id: &SessionId) -> Result<bool, RegistryError> {
        let (pinned, priority) = {
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
            let pinned = entry.session.pinned;
            let priority = entry
                .pty
                .clone()
                .map(|pty| (pty, !pinned && state.focused.as_ref() != Some(id)));
            (pinned, priority)
        };
        if let Some((pty, background)) = priority {
            if let Err(error) = pty.set_background(background) {
                tracing::warn!(
                    session_id = %id,
                    background,
                    error = %error,
                    "could not update process priority after pin change"
                );
            }
        }
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

    /// Attach the disk tier. Output from this point on is also spooled to
    /// `spool`, and [`Self::scrollback_history`] can reach past the ring.
    ///
    /// Set once, by whoever owns the data directory — the daemon. The registry
    /// deliberately does not choose a path itself: a library that picks its own
    /// place on disk is one a test cannot run twice at the same time.
    pub fn set_scrollback_spool(&self, spool: Arc<ScrollbackSpool>) {
        if let Ok(mut slot) = self.inner.spool.lock() {
            *slot = Some(spool);
        }
        self.rehydrate_scrollback();
    }

    /// Refill every restored session's memory ring from the disk tier.
    ///
    /// Once the log exists it is the durable copy, so the store stops carrying
    /// scrollback and this is what a restarted daemon replays into the focused
    /// pane. A session whose log is empty keeps whatever the store had, which
    /// is how a store written before the log existed still restores.
    fn rehydrate_scrollback(&self) {
        let Some(spool) = self.inner.spool() else {
            return;
        };
        let ids: Vec<SessionId> = {
            let state = lock_state(&self.inner);
            state.entries.keys().cloned().collect()
        };
        for id in ids {
            // Read files outside the lock; every session's output path needs it.
            let history = spool.history(&id, MAX_SCROLLBACK_BYTES as u64);
            if history.is_empty() {
                continue;
            }
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(&id) else {
                continue;
            };
            entry.scrollback = RingBuffer::default();
            entry.scrollback.push(&history);
            entry.grid = TerminalGrid::default();
            entry.grid.advance(&history);
        }
    }

    /// History for one session, newest bytes last, at most `max_bytes`.
    ///
    /// Answers from disk when a spool is attached and falls back to the memory
    /// ring otherwise, so a caller does not have to know which tier exists. The
    /// ring is *not* appended to the disk answer: every byte in it was spooled
    /// too, and concatenating would duplicate the most recent screenful.
    pub fn scrollback_history(
        &self,
        id: &SessionId,
        max_bytes: u64,
    ) -> Result<Vec<u8>, RegistryError> {
        {
            let state = lock_state(&self.inner);
            if !state.entries.contains_key(id) {
                return Err(RegistryError::Missing(id.clone()));
            }
        }
        // Read outside the state lock: this touches files, and every session's
        // output path goes through that lock.
        if let Some(spool) = self.inner.spool() {
            let history = spool.history(id, max_bytes);
            if !history.is_empty() {
                return Ok(history);
            }
        }
        let state = lock_state(&self.inner);
        let ring = state
            .entries
            .get(id)
            .map(|entry| entry.scrollback.to_vec())
            .unwrap_or_default();
        let start = ring.len().saturating_sub(max_bytes as usize);
        Ok(ring[start..].to_vec())
    }

    /// Return the parsed terminal state held for a background or pinned pane.
    /// The focused browser renderer does not need this path: it resets once and
    /// replays the raw bounded ring returned by [`Self::scrollback`].
    pub fn grid_snapshot(&self, id: &SessionId) -> Result<TerminalGridSnapshot, RegistryError> {
        let mut state = lock_state(&self.inner);
        state
            .entries
            .get_mut(id)
            .map(|entry| {
                // A session that opened a synchronized update and then stopped
                // writing has no further bytes to drive expiry, so the read side
                // has to do it or the pane renders a permanently stale frame.
                entry.grid.expire_sync_update();
                entry.grid.snapshot()
            })
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
        // Concurrently, not in series. Each stop is bounded by its own grace
        // period, and a fleet of thirty stopped one at a time would multiply
        // that by thirty on the daemon's own way out.
        let mut workers = Vec::new();
        for (id, generation, pty) in sessions {
            let registry = self.clone();
            let worker_id = id.clone();
            let worker_pty = pty.clone();
            match thread::Builder::new()
                .name(format!("terminalai-shutdown-stop-{id}"))
                .spawn(move || {
                    if let Ok(StopOutcome::Terminated) | Err(_) = worker_pty.stop() {
                        tracing::warn!(
                            session = %worker_id,
                            "agent did not shut down within its grace period during daemon shutdown"
                        );
                    }
                    registry.mark_process_exit(&worker_id, generation, None);
                }) {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    tracing::error!(
                        session = %id,
                        %error,
                        "could not start a shutdown stop worker; terminating the agent immediately"
                    );
                    let _ = pty.kill();
                    self.mark_process_exit(&id, generation, None);
                }
            }
        }
        for worker in workers {
            let _ = worker.join();
        }
    }

    fn pty(&self, id: &SessionId) -> Result<Arc<dyn AgentSession>, RegistryError> {
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
    ) -> Result<Arc<dyn AgentSession>, RegistryError> {
        let weak = Arc::downgrade(&self.inner);
        let callback_id = id.clone();
        let span = self.span_for(id);
        // Read once, here: the job is created with its limits, and a limit
        // applied after the process exists has a window it does not cover.
        let limits = lock_state(&self.inner).admission.job_limits();
        Ok(self.inner.domain.spawn(
            command,
            crate::pty::default_size(),
            environment,
            limits,
            Box::new(move |chunk| {
                let _entered = span.enter();
                if let Some(inner) = weak.upgrade() {
                    handle_output(&inner, &callback_id, generation, chunk);
                }
            }),
        )?)
    }

    fn emit_session(&self, id: &SessionId) {
        let session = {
            let state = lock_state(&self.inner);
            state.entries.get(id).map(|entry| entry.session.clone())
        };
        if let Some(session) = session {
            self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        }
    }

    fn emit_notification_changes(&self, changes: Vec<NotificationChange>) {
        for change in changes {
            if let Some(event) = change.into_event() {
                self.emit(RegistryEvent::Notification { event });
            }
        }
    }

    fn recheck_notifications(&self, now: SystemTime) {
        let (changes, stalled) = {
            let mut state = lock_state(&self.inner);
            // A stall is the absence of a transition, so nothing pushes it —
            // this periodic sweep is the only place it can be noticed. The flag
            // lives on the session so `fleet_order` stays a pure comparator.
            let mut stalled = Vec::new();
            for entry in state.entries.values_mut() {
                let is_stalled = entry.session.is_stalled_at(now);
                let mut changed = entry.session.stalled != is_stalled;
                if changed {
                    entry.session.stalled = is_stalled;
                    if is_stalled {
                        tracing::warn!(
                            parent: &entry.span,
                            held_for = ?now.duration_since(entry.session.status_since).unwrap_or_default(),
                            "session has held a working status past the stall threshold"
                        );
                    }
                }
                // The liveness verdict, which is a different question from the
                // stall flag above: that one asks how long a status has been
                // held, this one asks whether the process is still saying
                // anything at all. It reports and never restarts — a silent
                // agent may be thinking, and only a proven-dead process is
                // brought back.
                if entry.session.review_progress_at(now) {
                    tracing::warn!(
                        parent: &entry.span,
                        silent_for = ?entry.session.silent_for(now).unwrap_or_default(),
                        missed_deadlines = entry.session.missed_progress_deadlines,
                        "session has produced no output, transcript or hook event; \
                         marking it unresponsive without restarting it"
                    );
                    changed = true;
                }
                if changed {
                    stalled.push(entry.session.clone());
                }
            }
            let sessions: Vec<_> = state
                .entries
                .values()
                .map(|entry| entry.session.clone())
                .collect();
            (state.notifications.recheck(&sessions, now), stalled)
        };
        // Emitted outside the state lock, like every other session update.
        for session in stalled {
            self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        }
        self.emit_notification_changes(changes);
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

    /// Provision one session's declared lease.
    ///
    /// Ordered cheapest-first so a failure leaves as little behind as possible:
    /// copying files cannot fail halfway in a way that needs undoing, while the
    /// database is the one step that creates state outside the working tree.
    fn apply_lease(
        &self,
        id: &SessionId,
        lease: &lease::ResolvedLease,
        cwd: &std::path::Path,
        environment: &mut Vec<(String, String)>,
    ) -> Result<(), RegistryError> {
        // Copying is from the repository into itself for an in-place session,
        // which `copy_files` treats as a no-op; it matters once a session runs
        // in its own worktree.
        if !lease.copy.is_empty() {
            let source = self.lease_source(id).unwrap_or_else(|| cwd.to_path_buf());
            match lease::copy_files(&source, cwd, &lease.copy) {
                Ok(copied) if !copied.is_empty() => {
                    tracing::debug!(session = %id, "copied {} leased config file(s)", copied.len());
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(RegistryError::Environment(EnvironmentError::HookSpawn {
                        phase: "lease-copy",
                        cause: error.to_string(),
                    }))
                }
            }
        }

        let admin_url = lease
            .database
            .as_ref()
            .and_then(|database| std::env::var(&database.admin_url_env).ok());
        if let (Some(database), Some(args)) = (&lease.database, lease.create_database_args()) {
            let Some(admin) = admin_url.as_deref() else {
                // Declared but unprovisioned is refused rather than skipped: a
                // session that quietly falls back to the shared database is the
                // exact collision this lease exists to prevent.
                return Err(RegistryError::Environment(EnvironmentError::HookSpawn {
                    phase: "lease-database",
                    cause: format!(
                        "{} declares a database lease but {} is not set",
                        lease::LEASE_FILE,
                        database.admin_url_env
                    ),
                }));
            };
            run_lease_command("psql", cwd, &args, Some(admin), "lease-database")?;
        }

        for (key, value) in lease.variables(admin_url.as_deref()) {
            environment.retain(|(existing, _)| existing != &key);
            environment.push((key, value));
        }
        Ok(())
    }

    /// Where leased config is copied from. `None` when the session runs in the
    /// repository itself and there is nothing to copy across.
    /// Where leased config is copied *from*.
    ///
    /// A worktree is a fresh checkout, so it has the tracked files and none of
    /// the untracked ones — which is exactly where `.env` and its neighbours
    /// live. Copying from the repository the checkout was cut from is what
    /// makes an isolated session actually runnable.
    fn lease_source(&self, id: &SessionId) -> Option<std::path::PathBuf> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .and_then(|entry| entry.session.worktree.as_ref())
            .map(|worktree| worktree.repo.clone())
    }

    /// Attach the directory session worktrees are cut into.
    ///
    /// Unset means the registry has no place on disk it owns, so a session that
    /// asks for isolation is refused rather than checked out somewhere
    /// arbitrary. The daemon sets this; tests set it explicitly.
    pub fn set_worktree_root(&self, root: std::path::PathBuf) {
        if let Ok(mut slot) = self.inner.worktree_root.lock() {
            *slot = Some(root);
        }
    }

    /// Checkouts under the worktree root that no live session owns.
    ///
    /// Teardown deliberately keeps a branch holding unmerged work, which is
    /// right, but nothing ever revisited it — so worktrees, branches and their
    /// registrations accumulated silently. Reports rather than deletes: what to
    /// do about unmerged work is the operator's call, not the supervisor's.
    pub fn stale_worktrees(&self) -> Vec<crate::worktree::StaleWorktree> {
        let Some(root) = self
            .inner
            .worktree_root
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
        else {
            return Vec::new();
        };
        let live: Vec<crate::worktree::Worktree> = {
            let state = lock_state(&self.inner);
            state
                .entries
                .values()
                .filter_map(|entry| entry.session.worktree.clone())
                .collect()
        };
        crate::worktree::survey(&root, &live)
    }

    /// Create this session's worktree, if it asked for one.
    ///
    /// Runs on the worker thread that starts the session, because `git worktree
    /// add` copies a checkout and the launch call must not block on it.
    fn provision_worktree(&self, id: &SessionId) -> Result<(), RegistryError> {
        let (wanted, cwd) = {
            let state = lock_state(&self.inner);
            match state.entries.get(id) {
                // Already provisioned — a restart of an existing session must
                // reuse its checkout, not cut a second one.
                Some(entry) if entry.session.worktree.is_some() => return Ok(()),
                Some(entry) => (entry.spec.worktree, entry.spec.cwd.clone()),
                None => return Err(RegistryError::Missing(id.clone())),
            }
        };
        if !wanted {
            return Ok(());
        }
        let root = self
            .inner
            .worktree_root
            .lock()
            .ok()
            .and_then(|root| root.clone())
            .ok_or_else(|| {
                RegistryError::Environment(EnvironmentError::HookSpawn {
                    phase: "worktree",
                    cause: "no directory is configured for session worktrees".to_owned(),
                })
            })?;
        let created = crate::worktree::create(&root, &cwd, &id.0).map_err(|error| {
            // Refused, never downgraded to a shared tree: a session the
            // operator asked to isolate that quietly runs in the repository is
            // the collision this feature exists to prevent.
            RegistryError::Environment(EnvironmentError::HookSpawn {
                phase: "worktree",
                cause: error.to_string(),
            })
        })?;
        tracing::info!(
            path = %created.path.display(),
            branch = %created.branch,
            "session worktree created"
        );
        let session = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                // The session went away while git was working. Leaving the
                // checkout behind would orphan it, since nothing records it.
                drop(state);
                let failures = crate::worktree::remove(&created);
                if !failures.is_empty() {
                    tracing::warn!(?failures, "could not clean up an orphaned worktree");
                }
                return Err(RegistryError::Missing(id.clone()));
            };
            entry.session.cwd = created.path.clone();
            entry.session.branch = Some(created.branch.clone());
            entry.session.worktree = Some(created);
            entry.spec.cwd = entry.session.cwd.clone();
            entry.command.cwd = entry.session.cwd.clone();
            entry.session.clone()
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        Ok(())
    }

    /// Remove a session's checkout, returning what could not be cleaned up.
    fn release_worktree(&self, id: &SessionId) -> Vec<String> {
        let worktree = {
            let state = lock_state(&self.inner);
            state
                .entries
                .get(id)
                .and_then(|entry| entry.session.worktree.clone())
        };
        match worktree {
            Some(worktree) => crate::worktree::remove(&worktree),
            None => Vec::new(),
        }
    }

    /// Release a session's leased resources, returning every failure rather than
    /// the first: a compose stack that failed to come down must still be
    /// reported even if the database also failed to drop.
    fn release_lease(&self, id: &SessionId, cwd: &std::path::Path) -> Vec<String> {
        let mut failures = Vec::new();
        let lease = match lease::Lease::load(cwd) {
            Ok(Some(lease)) => lease,
            Ok(None) => return failures,
            Err(error) => {
                failures.push(format!("lease could not be re-read for teardown: {error}"));
                return failures;
            }
        };
        let resolved = match lease.resolve(&id.0, cwd) {
            Ok(resolved) => resolved,
            Err(error) => {
                failures.push(format!("lease could not be resolved for teardown: {error}"));
                return failures;
            }
        };

        if let Some(args) = resolved.compose_down_args() {
            if let Err(error) = run_lease_command("docker", cwd, &args, None, "lease-compose") {
                failures.push(error.to_string());
            }
        }
        if let Some(args) = resolved.drop_database_args() {
            let database = resolved.database.as_ref().expect("args imply a database");
            match std::env::var(&database.admin_url_env) {
                Ok(admin) => {
                    if let Err(error) =
                        run_lease_command("psql", cwd, &args, Some(&admin), "lease-database")
                    {
                        failures.push(error.to_string());
                    }
                }
                Err(_) => failures.push(format!(
                    "session database {} could not be dropped: {} is not set",
                    database.name, database.admin_url_env
                )),
            }
        }
        failures
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

    fn drain_queue(&self) {
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
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
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

fn handle_output(inner: &Arc<Inner>, id: &SessionId, generation: u64, bytes: &[u8]) {
    let (send_output, session, notifications) = {
        let mut state = lock_state(inner);
        let focused = state.focused.as_ref() == Some(id);
        let clear_operator_edit = state.entries.get(id).is_some_and(|entry| {
            entry.generation == generation && entry.session.status != SessionStatus::Idle
        });
        if clear_operator_edit {
            state.operator_edited.remove(id);
        }
        let Some(entry) = state.entries.get_mut(id) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        entry.scrollback.push(bytes);
        entry.grid.advance(bytes);
        // Any byte at all is evidence the process is alive, whether or not it
        // moves the status. A session can hold `Working` for an hour while
        // printing a build log the whole way, and nothing else on this path
        // would tell the supervisor the difference between that and a wedge.
        entry.session.note_progress_at(SystemTime::now());
        // Queued, never written here: this runs on the pty reader thread with
        // the state lock held, so a blocking write would stall every other
        // session and back-pressure the agent that produced the bytes.
        spool_append(inner, id, bytes);
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
        // PTY output also contains terminal-local echo. Only output observed
        // while the provider is in a non-idle state is strong enough evidence
        // to clear the operator-input guard.
        if clear_operator_edit {
            entry.queue.clear_focus_edit();
            entry.session.queued_prompts = entry.queue.len();
            entry.session.queue_paused = entry.queue.paused();
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
    emit_inner(inner, RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
    emit_notification_changes_inner(inner, notifications);
}

/// Hand bytes to the disk tier if one is attached.
///
/// Takes its own lock rather than the state lock, and never takes the state
/// lock, so the two can be held in either order without a cycle.
fn spool_append(inner: &Arc<Inner>, id: &SessionId, bytes: &[u8]) {
    if let Some(spool) = inner.spool() {
        spool.append(id, bytes);
    }
}

fn spool_forget(inner: &Arc<Inner>, id: &SessionId) {
    if let Some(spool) = inner.spool() {
        spool.forget(id);
    }
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

/// What the fleet holds right now, as the admission gate sees it.
///
/// Rate-limited sessions are excluded by `occupies_admission_slot`: they are
/// running, but the provider is refusing them work, so counting them would keep
/// a queued session waiting behind a process that provably cannot progress.
fn admitted_demand(state: &State) -> FleetDemand {
    let mut demand = FleetDemand::default();
    for entry in state.entries.values() {
        if entry.session.status.occupies_admission_slot() {
            demand.admit(entry.session.agent, entry.session.memory_bytes);
        }
    }
    demand
}

/// The gate's answer for this fleet. Reading the clock is the registry's job:
/// [`crate::admission::block`] is given the spend already resolved so it stays a
/// pure function of what it is handed.
fn admission_block(state: &State) -> Option<AdmissionBlock> {
    let mut demand = admitted_demand(state);
    demand.spend_window_usd = state
        .spend
        .window_total_at(SystemTime::now(), state.admission.spend_window);
    crate::admission::block(&state.admission, &demand)
}

fn projected_memory_bytes(state: &State) -> u64 {
    admitted_demand(state).projected_memory_bytes
}

fn admitted_count(state: &State) -> usize {
    admitted_demand(state).admitted
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

/// Run one leased provisioning or teardown command.
///
/// `PGDATABASE`-style connection details are passed through the environment
/// rather than the argument vector so a connection string carrying a password
/// never appears in a process listing.
fn run_lease_command(
    program: &str,
    cwd: &std::path::Path,
    args: &[String],
    connection: Option<&str>,
    phase: &'static str,
) -> Result<(), RegistryError> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let extra_environment = connection
        .map(|connection| {
            vec![
                // libpq treats PGDATABASE as its dbname default, including
                // URI/keyword connection strings. PGURI is not a libpq key.
                ("PGDATABASE".to_owned(), connection.to_owned()),
                ("PGCONNECT_TIMEOUT".to_owned(), "10".to_owned()),
            ]
        })
        .unwrap_or_default();
    environment::configure_command_environment(&mut command, &extra_environment);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command
        .output()
        .map_err(|error| EnvironmentError::HookSpawn {
            phase,
            cause: format!("could not run {program}: {error}"),
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(RegistryError::Environment(EnvironmentError::HookSpawn {
        phase,
        cause: if detail.is_empty() {
            format!("{program} exited with {}", output.status)
        } else {
            format!("{program}: {detail}")
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentBinary, Origin};
    use crate::app_server::{AppServerEvent, AppServerThreadStatus, AppServerTokenUsage};
    use crate::domain::{AgentDomain, AgentSession, DomainError, OutputHandler};
    use crate::launch::spec_for;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct RecordingDomain {
        spawns: Arc<AtomicUsize>,
        commands: Arc<Mutex<Vec<ResolvedCommand>>>,
    }

    struct ExitedSession;

    impl AgentSession for ExitedSession {
        fn write(&self, _bytes: &[u8]) -> Result<(), DomainError> {
            Err(DomainError::Message("session is not writable".into()))
        }

        fn resize(&self, _size: PtySize) -> Result<(), DomainError> {
            Err(DomainError::Message("session is not resizable".into()))
        }

        fn pid(&self) -> Option<u32> {
            None
        }

        fn try_wait(&self) -> Result<Option<u32>, DomainError> {
            Ok(Some(0))
        }

        fn wait_for_exit(&self) -> Result<u32, DomainError> {
            Err(DomainError::Message("remote wait is unavailable".into()))
        }

        fn kill(&self) -> Result<(), DomainError> {
            Ok(())
        }
    }

    impl AgentDomain for RecordingDomain {
        fn spawn(
            &self,
            command: &ResolvedCommand,
            _size: PtySize,
            _environment: &[(String, String)],
            _limits: crate::process_tree::JobLimits,
            _on_output: OutputHandler,
        ) -> Result<Arc<dyn AgentSession>, DomainError> {
            // Record before signalling: the test waits on `spawns` and then
            // reads `commands`, so incrementing first leaves a window where the
            // counter says a spawn happened and the command is not there yet.
            self.commands.lock().unwrap().push(command.clone());
            self.spawns.fetch_add(1, Ordering::Release);
            Ok(Arc::new(ExitedSession))
        }
    }

    #[test]
    fn registry_launch_uses_an_injected_domain_without_local_process_access() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let registry = SessionRegistry::with_domain(Arc::new(RecordingDomain {
            spawns: spawns.clone(),
            commands: commands.clone(),
        }));
        let cwd = std::env::current_dir().expect("cwd");
        let spec = spec_for(Agent::Claude, &cwd);
        registry
            .launch(
                spec,
                AgentBinary {
                    agent: Agent::Claude,
                    path: PathBuf::from("claude.exe"),
                    origin: Origin::Configured,
                },
            )
            .expect("injected domain launch");

        let deadline = Instant::now() + Duration::from_secs(2);
        while spawns.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(spawns.load(Ordering::Acquire), 1);
        let commands = commands.lock().unwrap();
        let session_id = commands[0]
            .args
            .windows(2)
            .find(|window| window[0] == "--session-id")
            .map(|window| window[1].as_str())
            .expect("new Claude launches carry an explicit transcript id");
        assert!(crate::launch::is_valid_resume_id(session_id));
    }

    #[test]
    fn lease_command_child_probe() {
        let cwd = std::env::current_dir().expect("probe cwd");
        let marker = cwd.join("terminalai-lease-command-probe.request");
        if !marker.exists() {
            return;
        }
        let report = serde_json::json!({
            "args": std::env::args().collect::<Vec<_>>(),
            "environment": std::env::vars().collect::<std::collections::BTreeMap<_, _>>(),
        });
        std::fs::write(
            cwd.join("terminalai-lease-command-probe.json"),
            serde_json::to_vec(&report).expect("encode probe report"),
        )
        .expect("write probe report");
    }

    #[test]
    fn lease_command_uses_the_allowlist_without_putting_connection_in_argv() {
        let scratch = spool_scratch("lease-command");
        std::fs::create_dir_all(&scratch.0).expect("scratch directory");
        std::fs::write(
            scratch.0.join("terminalai-lease-command-probe.request"),
            "probe",
        )
        .expect("probe marker");

        let connection = "postgresql://admin:password@127.0.0.1:5432/postgres?sslmode=require";
        let executable = std::env::current_exe().expect("test executable");
        run_lease_command(
            executable.to_str().expect("test executable path"),
            &scratch.0,
            &["lease_command_child_probe".to_owned()],
            Some(connection),
            "lease-test",
        )
        .expect("spawn lease command probe");

        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(scratch.0.join("terminalai-lease-command-probe.json"))
                .expect("probe report"),
        )
        .expect("decode probe report");
        let child_args = report["args"].as_array().expect("child argv");
        assert!(
            !child_args
                .iter()
                .any(|argument| argument.as_str() == Some(connection)),
            "connection string leaked into child argv: {child_args:?}"
        );

        let child_environment = report["environment"]
            .as_object()
            .expect("child environment");
        let allowed = environment::safe_environment_keys()
            .iter()
            .copied()
            .chain(["PGDATABASE", "PGCONNECT_TIMEOUT"])
            .collect::<std::collections::BTreeSet<_>>();
        let unexpected = child_environment
            .keys()
            // The profiling runtime sets its own variables inside the child,
            // after `env_clear()` has already run — so they are not inherited
            // and say nothing about the allowlist. Without this the suite cannot
            // run under `cargo llvm-cov` at all, and narrowing the assertion to
            // this one prefix keeps it strict about everything that is actually
            // passed in.
            .filter(|key| !key.starts_with("__LLVM_PROFILE") && key.as_str() != "LLVM_PROFILE_FILE")
            .filter(|key| !allowed.contains(key.as_str()))
            .collect::<Vec<_>>();
        assert!(
            unexpected.is_empty(),
            "lease command inherited unexpected environment keys: {unexpected:?}"
        );
        assert_eq!(
            child_environment["PGDATABASE"].as_str(),
            Some(connection)
        );
        assert_eq!(
            child_environment["PGCONNECT_TIMEOUT"].as_str(),
            Some("10")
        );
        assert!(!child_environment.contains_key("PGURI"));
    }

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
                session: Box::new(session.clone()),
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

    /// Build a live entry occupying an admission slot, optionally with a
    /// sampled memory figure.
    fn live_entry(
        registry: &SessionRegistry,
        id: SessionId,
        agent: Agent,
        memory_bytes: Option<u64>,
    ) {
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(agent, &cwd);
        let mut session = Session::new(id.clone(), &spec);
        session.status = SessionStatus::Working;
        session.phase = SessionPhase::Working;
        session.memory_bytes = memory_bytes;
        lock_state(&registry.inner).entries.insert(
            id,
            Entry {
                session,
                spec,
                command: ResolvedCommand {
                    program: PathBuf::from("agent"),
                    args: Vec::new(),
                    cwd,
                },
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                queue: crate::queue::PromptQueue::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );
    }

    #[test]
    fn an_unsampled_session_is_projected_at_its_agents_measured_size() {
        // Admitting on "we have not looked yet" is how a machine gets
        // oversubscribed, so a session with no sample still counts.
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(8, None));
        live_entry(&registry, SessionId::new(1), Agent::Claude, None);
        live_entry(&registry, SessionId::new(2), Agent::Codex, None);
        let state = lock_state(&registry.inner);
        assert_eq!(
            projected_memory_bytes(&state),
            ASSUMED_SESSION_BYTES_CLAUDE + ASSUMED_SESSION_BYTES_CODEX
        );
    }

    #[test]
    fn a_sampled_session_is_projected_at_what_it_actually_uses() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(8, None));
        live_entry(&registry, SessionId::new(1), Agent::Claude, Some(64 * 1024 * 1024));
        let state = lock_state(&registry.inner);
        assert_eq!(projected_memory_bytes(&state), 64 * 1024 * 1024);
    }

    #[test]
    fn the_memory_budget_blocks_admission_while_slots_are_still_free() {
        let registry = SessionRegistry::with_admission(
            AdmissionConfig::new(8, None).with_memory_limits(
                Some(600 * 1024 * 1024),
                None,
                None,
            ),
        );
        live_entry(&registry, SessionId::new(1), Agent::Claude, None);
        let snapshot = registry.admission_snapshot();
        assert_eq!(snapshot.admission_block, Some(AdmissionBlock::MemoryBudget));
        assert!(
            snapshot.live_sessions < snapshot.max_live_sessions,
            "slots are free; memory is what is blocking"
        );
        assert_eq!(snapshot.memory_budget_bytes, Some(600 * 1024 * 1024));
        assert_eq!(snapshot.projected_memory_bytes, ASSUMED_SESSION_BYTES_CLAUDE);
    }

    #[test]
    fn an_empty_fleet_always_gets_one_session_even_under_a_tiny_budget() {
        // A budget too small for any agent is a misconfiguration; halting the
        // fleet entirely would hide it rather than surface it.
        let registry = SessionRegistry::with_admission(
            AdmissionConfig::new(8, None).with_memory_limits(Some(1024), None, None),
        );
        assert_eq!(registry.admission_snapshot().admission_block, None);
    }

    #[test]
    fn no_memory_budget_never_blocks_admission() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(8, None));
        for index in 1..=4 {
            live_entry(&registry, SessionId::new(index), Agent::Claude, None);
        }
        let snapshot = registry.admission_snapshot();
        assert_eq!(snapshot.admission_block, None);
        assert!(snapshot.projected_memory_bytes > 0, "projection is still reported");
    }

    #[test]
    fn a_zero_limit_disables_rather_than_admitting_nothing() {
        // A misconfigured limit must not halt the fleet.
        let config = AdmissionConfig::new(4, None).with_memory_limits(Some(0), Some(0), Some(0));
        assert_eq!(config.memory_budget_bytes, None);
        assert_eq!(config.session_memory_cap_bytes, None);
        assert_eq!(config.max_processes_per_session, None);
        assert_eq!(config.job_limits(), crate::process_tree::JobLimits::default());
    }

    #[test]
    fn the_session_cap_becomes_the_jobs_limits() {
        let config = AdmissionConfig::new(4, None).with_memory_limits(
            None,
            Some(2 * 1024 * 1024 * 1024),
            Some(64),
        );
        let limits = config.job_limits();
        assert_eq!(limits.memory_bytes, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(limits.active_processes, Some(64));
    }

    #[test]
    fn the_spend_ceiling_refuses_new_admissions_and_leaves_running_sessions_alone() {
        let registry = SessionRegistry::with_admission(
            AdmissionConfig::new(8, None)
                .with_spend_ceiling(Some(10.0), Some(Duration::from_secs(3600))),
        );
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);

        // A running row that the ceiling must not touch.
        let active_id = SessionId::new(99);
        let mut active = Session::new(active_id.clone(), &spec);
        active.status = SessionStatus::Working;
        active.phase = SessionPhase::Working;
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
                queue: crate::queue::PromptQueue::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );

        // Under the ceiling there is nothing stopping a launch.
        assert_eq!(registry.admission_snapshot().admission_block, None);

        lock_state(&registry.inner)
            .spend
            .record_at(SystemTime::now(), 12.0);

        let snapshot = registry.admission_snapshot();
        assert_eq!(
            snapshot.admission_block,
            Some(AdmissionBlock::SpendCeiling),
            "the ceiling, not the slot cap, is what is blocking"
        );
        assert!(snapshot.live_sessions < snapshot.max_live_sessions, "slots are free");
        assert_eq!(snapshot.spend_ceiling_usd, Some(10.0));
        assert!((snapshot.spend_window_usd - 12.0).abs() < 1e-9);

        let queued_id = registry
            .launch(
                spec,
                AgentBinary {
                    agent: Agent::Claude,
                    path: PathBuf::from("missing-terminalai-test-agent.exe"),
                    origin: Origin::Path,
                },
            )
            .expect("a launch over the ceiling is queued, not rejected outright");
        assert!(registry.is_queued(&queued_id).expect("queued row"));
        assert_eq!(
            lock_state(&registry.inner).entries[&active_id].session.status,
            SessionStatus::Working,
            "a running session is never stopped by the ceiling"
        );
    }

    #[test]
    fn a_ceiling_of_none_never_blocks_admission() {
        let registry = SessionRegistry::with_admission(
            AdmissionConfig::new(8, None).with_spend_ceiling(None, None),
        );
        lock_state(&registry.inner)
            .spend
            .record_at(SystemTime::now(), 5_000.0);
        let snapshot = registry.admission_snapshot();
        assert_eq!(snapshot.admission_block, None);
        assert_eq!(snapshot.spend_ceiling_usd, None);
        assert!(snapshot.spend_window_usd > 0.0, "spend is still reported");
    }

    #[test]
    fn the_slot_cap_is_reported_ahead_of_the_ceiling() {
        // Both limits are hit; the operator is told about the one they can act
        // on immediately rather than a arbitrary pick between the two.
        let registry = SessionRegistry::with_admission(
            AdmissionConfig::new(1, None).with_spend_ceiling(Some(1.0), None),
        );
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let active_id = SessionId::new(1);
        let mut active = Session::new(active_id.clone(), &spec);
        active.status = SessionStatus::Working;
        active.phase = SessionPhase::Working;
        lock_state(&registry.inner).entries.insert(
            active_id,
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
                queue: crate::queue::PromptQueue::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );
        lock_state(&registry.inner)
            .spend
            .record_at(SystemTime::now(), 9.0);
        assert_eq!(
            registry.admission_snapshot().admission_block,
            Some(AdmissionBlock::SlotsFull)
        );
    }

    #[test]
    fn spend_survives_a_store_round_trip() {
        let registry = SessionRegistry::with_admission(
            AdmissionConfig::new(4, None).with_spend_ceiling(Some(10.0), None),
        );
        lock_state(&registry.inner)
            .spend
            .record_at(SystemTime::now(), 7.5);
        let snapshot = registry.store_snapshot();
        assert!(!snapshot.spend.is_empty(), "the ledger is persisted");

        let restored = SessionRegistry::from_store_with_admission(
            snapshot,
            AdmissionConfig::new(4, None).with_spend_ceiling(Some(10.0), None),
        );
        assert!(
            (restored.admission_snapshot().spend_window_usd - 7.5).abs() < 1e-9,
            "restarting the daemon does not clear the ceiling"
        );
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
                queue: crate::queue::PromptQueue::default(),
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
                queue: crate::queue::PromptQueue::default(),
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
                queue: crate::queue::PromptQueue::default(),
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

    /// A row with no process behind it, so archiving is reachable without
    /// spawning anything.
    fn restored_row(id: u64) -> SessionRegistry {
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let session = Session::new(SessionId::new(id), &spec);
        SessionRegistry::from_store(SessionStoreSnapshot {
            magic: crate::store::SESSION_STORE_MAGIC.to_owned(),
            schema_version: crate::store::SESSION_STORE_SCHEMA_VERSION,
            spend: Vec::new(),
            sessions: vec![StoredSession {
                session,
                spec,
                command: ResolvedCommand {
                    program: PathBuf::from("claude.exe"),
                    args: Vec::new(),
                    cwd,
                },
                scrollback: Vec::new(),
                queue: Default::default(),
            }],
            archives: Vec::new(),
            extra: BTreeMap::new(),
        })
    }

    fn a_landing(files_changed: usize) -> crate::land::Landing {
        crate::land::Landing {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000),
            target: PathBuf::from("C:/repos/project"),
            target_head: "abc1234".into(),
            files_changed,
            verified: Some(true),
        }
    }

    #[test]
    fn a_landing_is_recorded_on_the_row_and_survives_into_the_archive() {
        let registry = restored_row(11);
        let id = SessionId::new(11);
        registry
            .record_landing(&id, a_landing(3))
            .expect("record the landing");

        let row = registry
            .snapshot()
            .into_iter()
            .find(|session| session.id == id)
            .expect("the row");
        assert_eq!(row.landed.as_ref().expect("landed").files_changed, 3);

        // The archive is where the question is actually asked: the row is gone
        // by the time anyone surveys leftover checkouts.
        let archived = registry.archive(&id).expect("archive");
        assert_eq!(archived.landed.expect("the landing"), a_landing(3));
        assert_eq!(
            registry.archives()[0].landed.as_ref().expect("landed").target_head,
            "abc1234"
        );
    }

    #[test]
    fn a_session_that_never_landed_is_archived_as_one_that_never_landed() {
        // The distinction is only worth anything if the absence is recorded as
        // faithfully as the presence: this is the abandoned case.
        let registry = restored_row(12);
        let archived = registry.archive(&SessionId::new(12)).expect("archive");
        assert!(archived.landed.is_none());
    }

    #[test]
    fn recording_a_landing_against_a_row_that_is_gone_is_an_error_not_a_panic() {
        let registry = restored_row(13);
        assert!(registry
            .record_landing(&SessionId::new(99), a_landing(1))
            .is_err());
    }

    #[test]
    fn store_restore_keeps_rows_and_replay_bytes_without_starting_processes() {
        let cwd = Path::new(".").to_path_buf();
        let mut spec = spec_for(Agent::Claude, &cwd);
        spec.resume = Resume::Session("native-1".into());
        let mut session = Session::new(SessionId::new(7), &spec);
        session.resume_id = Some("native-1".into());
        let snapshot = SessionStoreSnapshot {
            magic: crate::store::SESSION_STORE_MAGIC.to_owned(),
            schema_version: crate::store::SESSION_STORE_SCHEMA_VERSION,
            spend: Vec::new(),
            sessions: vec![StoredSession {
                session,
                spec,
                command: ResolvedCommand {
                    program: PathBuf::from("claude.exe"),
                    args: vec!["--resume".into(), "native-1".into()],
                    cwd,
                },
                scrollback: b"restored\r\n".to_vec(),
                queue: Default::default(),
            }],
            archives: Vec::new(),
            extra: BTreeMap::from([("future_field".into(), serde_json::json!({"retained": true}))]),
        };

        let registry = SessionRegistry::from_store(snapshot);
        let session = &registry.snapshot()[0];
        assert_eq!(session.id, SessionId::new(7));
        assert_eq!(session.phase, SessionPhase::Resurrectable);
        assert_eq!(session.status, SessionStatus::Exited);
        assert!(session.pid.is_none());
        assert_eq!(
            registry.store_snapshot().extra["future_field"]["retained"],
            true
        );
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
                    queue: crate::queue::PromptQueue::default(),
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
        assert!(
            stored.archives[0].archived_at.is_some(),
            "an archive written now carries the stamp its age bound is measured against"
        );
    }

    #[test]
    fn a_full_archive_list_stays_at_its_bound_when_another_row_is_archived() {
        // The store-level tests prove `trim_archives`; this one proves the live
        // archive path actually calls it, which is the part that regresses.
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let now = SystemTime::now();
        let snapshot = SessionStoreSnapshot {
            archives: (0..crate::store::MAX_ARCHIVES as u64)
                .map(|sequence| ArchivedSession {
                    id: SessionId::new(sequence + 1),
                    agent: Agent::Claude,
                    name: format!("row-{sequence}"),
                    cwd: cwd.clone(),
                    command: "claude.exe".into(),
                    archived_at: Some(now),
                    landed: None,
                })
                .collect(),
            ..Default::default()
        };
        let registry = SessionRegistry::from_store(snapshot);
        let id = SessionId::new(9_000);
        let session = Session::new(id.clone(), &spec);
        {
            let mut state = lock_state(&registry.inner);
            state.entries.insert(
                id.clone(),
                Entry {
                    session,
                    command: ResolvedCommand {
                        program: PathBuf::from("claude.exe"),
                        args: Vec::new(),
                        cwd: cwd.clone(),
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
        registry.archive(&id).expect("archive");

        let stored = registry.store_snapshot();
        assert_eq!(stored.archives.len(), crate::store::MAX_ARCHIVES);
        assert_eq!(
            stored.archives[crate::store::MAX_ARCHIVES - 1].id,
            id,
            "the row just archived is the one that was kept"
        );
        assert!(
            !stored.archives.iter().any(|item| item.id == SessionId::new(1)),
            "the oldest record is the one that went"
        );
    }

    #[test]
    fn the_history_is_newest_first_and_carries_the_exact_command() {
        let cwd = Path::new(".").to_path_buf();
        let now = SystemTime::now();
        let snapshot = SessionStoreSnapshot {
            archives: (0..3u64)
                .map(|sequence| ArchivedSession {
                    id: SessionId::new(sequence + 1),
                    agent: Agent::Claude,
                    name: format!("row-{sequence}"),
                    cwd: cwd.clone(),
                    command: format!("claude.exe --model opus-{sequence}"),
                    archived_at: Some(now),
                    landed: None,
                })
                .collect(),
            ..Default::default()
        };
        let registry = SessionRegistry::from_store(snapshot);
        let history = registry.archives();
        assert_eq!(history.len(), 3);
        // The store appends, so the last archived row is the one the operator is
        // most likely to want back — it must not be at the bottom of the list.
        assert_eq!(history[0].id, SessionId::new(3));
        assert_eq!(history[2].id, SessionId::new(1));
        assert_eq!(history[0].command, "claude.exe --model opus-2");
    }

    #[test]
    fn an_oversized_store_is_brought_inside_the_bound_without_reusing_an_id() {
        let cwd = Path::new(".").to_path_buf();
        let now = SystemTime::now();
        let count = crate::store::MAX_ARCHIVES as u64 + 25;
        let snapshot = SessionStoreSnapshot {
            archives: (0..count)
                .map(|sequence| ArchivedSession {
                    id: SessionId::new(sequence + 1),
                    agent: Agent::Claude,
                    name: format!("row-{sequence}"),
                    cwd: cwd.clone(),
                    command: "claude.exe".into(),
                    archived_at: Some(now),
                    landed: None,
                })
                .collect(),
            ..Default::default()
        };
        let registry = SessionRegistry::from_store(snapshot);
        let stored = registry.store_snapshot();
        assert_eq!(stored.archives.len(), crate::store::MAX_ARCHIVES);
        // The trimmed records took their ids with them, but the next id must
        // still clear the highest one ever issued.
        assert_eq!(lock_state(&registry.inner).next_id, count + 1);
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
                    queue: crate::queue::PromptQueue::default(),
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
                    queue: crate::queue::PromptQueue::default(),
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
                queue: crate::queue::PromptQueue::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );

        assert!(apply_test_hook(&registry, HookEvent {
            agent: Agent::Claude,
            session_id: Some("--dangerously-skip-permissions".into()),
            cwd: Some(cwd.clone()),
            signal: HookSignal::SessionStart,
            progress: None,
        }));
        assert_eq!(
            registry.snapshot()[0].resume_id,
            None,
            "a flag-like hook id must not become a resume id"
        );

        assert!(apply_test_hook(&registry, HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd.clone()),
            signal: HookSignal::SessionStart,
            progress: None,
        }));
        assert!(apply_test_hook(&registry, HookEvent {
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

        assert!(apply_test_hook(&registry, HookEvent {
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
                queue: crate::queue::PromptQueue::default(),
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

        assert!(apply_test_hook(&registry, attention.clone()));
        let first: Vec<_> = events.try_iter().collect();
        assert!(first.iter().any(|event| matches!(
            event,
            RegistryEvent::Notification {
                event: NotificationEvent::Raised { notification }
            } if notification.dedup_key.contains("session=s0001")
        )));

        assert!(apply_test_hook(&registry, attention));
        assert!(!events
            .try_iter()
            .any(|event| matches!(event, RegistryEvent::Notification { .. })));

        assert!(apply_test_hook(&registry, HookEvent {
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
    fn a_hook_cannot_bind_to_a_session_it_did_not_come_from() {
        let registry = SessionRegistry::new();
        let first = SessionId::new(1);
        let second = SessionId::new(2);
        insert_session(&registry, &first, SessionStatus::Starting);
        insert_session(&registry, &second, SessionStatus::Starting);
        let first_token = {
            let state = lock_state(&registry.inner);
            state
                .entries
                .get(&first)
                .map(|entry| entry.session.hook_token.clone())
                .expect("first hook token")
        };
        let cwd = Path::new(".").to_path_buf();

        assert!(registry.apply_hook_with_token(
            HookEvent {
                agent: Agent::Claude,
                session_id: Some("native-first".into()),
                cwd: Some(cwd.clone()),
                signal: HookSignal::SessionStart,
                progress: None,
            },
            Some(&first_token),
        ));
        assert!(!registry.apply_hook_with_token(
            HookEvent {
                agent: Agent::Claude,
                session_id: Some("native-second".into()),
                cwd: Some(cwd),
                signal: HookSignal::SessionStart,
                progress: None,
            },
            Some(&first_token),
        ));

        let sessions = registry.snapshot();
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == first)
                .and_then(|session| session.resume_id.as_deref()),
            Some("native-first")
        );
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == second)
                .and_then(|session| session.resume_id.as_deref()),
            None
        );
    }

    #[test]
    fn hooks_from_other_sessions_are_ignored() {
        let registry = SessionRegistry::new();
        assert!(!apply_test_hook(&registry, HookEvent {
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
                queue: crate::queue::PromptQueue::default(),
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

    /// Insert a session in a chosen state without spawning a process.
    fn insert_session(registry: &SessionRegistry, id: &SessionId, status: SessionStatus) {
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let mut session = Session::new(id.clone(), &spec);
        session.status = status;
        lock_state(&registry.inner).entries.insert(
            id.clone(),
            Entry {
                session,
                spec,
                command: ResolvedCommand {
                    program: PathBuf::from("agent"),
                    args: Vec::new(),
                    cwd,
                },
                pty: None,
                scrollback: RingBuffer::default(),
                grid: TerminalGrid::default(),
                queue: crate::queue::PromptQueue::default(),
                generation: 1,
                stop_requested: false,
                teardown_done: true,
                branch_checked: None,
                span: tracing::Span::none(),
            },
        );
    }

    /// Unit tests model the adapter boundary by retrieving the private token
    /// for the only matching row, then sending it through the same authenticated
    /// registry entry point as the daemon.
    fn apply_test_hook(registry: &SessionRegistry, event: HookEvent) -> bool {
        let token = {
            let state = lock_state(&registry.inner);
            state
                .entries
                .values()
                .rfind(|entry| {
                    entry.session.agent == event.agent
                        && event
                            .cwd
                            .as_ref()
                            .is_none_or(|cwd| cwd == &entry.session.cwd)
                        && (event.session_id.as_deref().is_none()
                            || entry.session.resume_id.as_deref() == event.session_id.as_deref()
                            || entry.session.resume_id.is_none())
                })
                .map(|entry| entry.session.hook_token.clone())
        };
        registry.apply_hook_with_token(event, token.as_deref())
    }

    fn rate_limit_event(resets_in_seconds: u64) -> HookEvent {
        HookEvent {
            agent: Agent::Claude,
            session_id: None,
            cwd: Some(Path::new(".").to_path_buf()),
            signal: HookSignal::RateLimited {
                limit: crate::hooks::HookRateLimit {
                    scope: "rate-limit".to_owned(),
                    used_percent: Some(100.0),
                    window_minutes: Some(300),
                    resets_in_seconds: Some(resets_in_seconds),
                    resets_at_unix: None,
                    plan: Some("max".to_owned()),
                },
            },
            progress: None,
        }
    }

    #[test]
    fn a_rate_limited_session_releases_its_admission_slot() {
        // The failure this prevents: a fleet at its cap, every session waiting
        // on a quota, and a queued session that never starts because the
        // blocked ones still count as live.
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(2, None));
        let limited = SessionId::new(1);
        insert_session(&registry, &limited, SessionStatus::Working);
        insert_session(&registry, &SessionId::new(2), SessionStatus::Working);
        assert_eq!(registry.admission_snapshot().live_sessions, 2);

        assert!(apply_test_hook(&registry, rate_limit_event(1800)));

        let snapshot = registry.admission_snapshot();
        assert_eq!(
            snapshot.live_sessions, 1,
            "a session the provider is refusing must not hold a slot"
        );
        assert_eq!(snapshot.rate_limited_sessions, 1);
        assert!(snapshot.earliest_rate_limit_reset.is_some());
    }

    #[test]
    fn a_rate_limited_row_sorts_above_the_working_states() {
        // It sorts with the states that want the operator's attention, because a
        // limited session is indistinguishable from a busy one at a glance.
        assert!(SessionStatus::RateLimited > SessionStatus::Working);
        assert!(SessionStatus::RateLimited > SessionStatus::Thinking);
        assert!(SessionStatus::RateLimited > SessionStatus::Idle);
        assert!(SessionStatus::RateLimited < SessionStatus::NeedsApproval);
    }

    #[test]
    fn the_header_reports_the_earliest_reset_across_the_fleet() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(4, None));
        insert_session(&registry, &SessionId::new(1), SessionStatus::Working);
        assert!(apply_test_hook(&registry, rate_limit_event(7200)));
        let far = registry
            .admission_snapshot()
            .earliest_rate_limit_reset
            .expect("a reset was reported");

        insert_session(&registry, &SessionId::new(2), SessionStatus::Working);
        assert!(apply_test_hook(&registry, rate_limit_event(60)));

        let snapshot = registry.admission_snapshot();
        assert_eq!(snapshot.rate_limited_sessions, 2);
        assert!(
            snapshot.earliest_rate_limit_reset.expect("still reported") < far,
            "the header must show the soonest reset, not the first one seen"
        );
    }

    #[test]
    fn the_limited_state_is_never_entered_from_silence() {
        // Every other status here can be reached by inference. This one must not
        // be: a quiet session is indistinguishable from a long tool call, and a
        // fleet that invents quota states is worse than one that shows none.
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(2, None));
        insert_session(&registry, &SessionId::new(1), SessionStatus::Working);

        for signal in [
            HookSignal::Stop,
            HookSignal::PostToolUse,
            HookSignal::Unknown {
                event: "idle_timeout".to_owned(),
            },
            HookSignal::Notification {
                notification: crate::hooks::HookNotification::IdlePrompt,
            },
        ] {
            apply_test_hook(&registry, HookEvent {
                agent: Agent::Claude,
                session_id: None,
                cwd: Some(Path::new(".").to_path_buf()),
                signal,
                progress: None,
            });
            assert_ne!(
                registry.snapshot()[0].status,
                SessionStatus::RateLimited,
                "no signal short of a reported quota may produce this state"
            );
        }
        assert_eq!(registry.admission_snapshot().rate_limited_sessions, 0);
    }

    #[test]
    fn a_reset_window_returns_the_row_to_the_fleet() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(2, None));
        insert_session(&registry, &SessionId::new(1), SessionStatus::Working);
        // A window that has already elapsed by the time the next event lands.
        assert!(apply_test_hook(&registry, rate_limit_event(0)));
        assert_eq!(registry.snapshot()[0].status, SessionStatus::RateLimited);

        std::thread::sleep(Duration::from_millis(20));
        apply_test_hook(&registry, HookEvent {
            agent: Agent::Claude,
            session_id: None,
            cwd: Some(Path::new(".").to_path_buf()),
            signal: HookSignal::PostToolUse,
            progress: None,
        });

        assert_ne!(registry.snapshot()[0].status, SessionStatus::RateLimited);
        assert!(registry.snapshot()[0].rate_limit.is_none());
        assert_eq!(registry.admission_snapshot().live_sessions, 1);
    }

    #[test]
    fn a_quota_report_with_room_left_clears_the_limit() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(2, None));
        insert_session(&registry, &SessionId::new(1), SessionStatus::Working);
        assert!(apply_test_hook(&registry, rate_limit_event(9_000)));
        assert_eq!(registry.snapshot()[0].status, SessionStatus::RateLimited);

        // Positive evidence, not silence: the provider answered with a window
        // that still has room.
        apply_test_hook(&registry, HookEvent {
            agent: Agent::Claude,
            session_id: None,
            cwd: Some(Path::new(".").to_path_buf()),
            signal: HookSignal::RateLimitCleared {
                limit: crate::hooks::HookRateLimit {
                    scope: "primary".into(),
                    used_percent: Some(12.0),
                    window_minutes: Some(300),
                    resets_in_seconds: Some(600),
                    resets_at_unix: None,
                    plan: None,
                },
            },
            progress: None,
        });
        assert_ne!(registry.snapshot()[0].status, SessionStatus::RateLimited);
        assert!(registry.snapshot()[0].rate_limit.is_none());
    }

    struct SpoolScratch(PathBuf);

    impl Drop for SpoolScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spool_scratch(name: &str) -> SpoolScratch {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-registry-spool-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        SpoolScratch(dir)
    }

    /// Drive output through the same path the pty reader uses.
    fn feed(registry: &SessionRegistry, id: &SessionId, bytes: &[u8]) {
        let generation = lock_state(&registry.inner)
            .entries
            .get(id)
            .map(|entry| entry.generation)
            .expect("entry");
        handle_output(&registry.inner, id, generation, bytes);
    }

    #[test]
    fn history_reaches_past_what_the_memory_ring_kept() {
        // The reason the disk tier exists. The ring is deliberately small, so
        // an agent that produced more than a megabyte of output has already
        // lost the beginning by the time anyone asks.
        let dir = spool_scratch("past-ring");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Working);
        registry.set_scrollback_spool(Arc::new(
            ScrollbackSpool::new(&dir.0).expect("spool"),
        ));

        feed(&registry, &id, &vec![b'x'; 500 * 1024]);
        let marker = b"THE FIRST THING IT SAID\n";
        feed(&registry, &id, marker);
        feed(&registry, &id, &vec![b'y'; 600 * 1024]);
        registry.inner.spool().expect("spool").flush();

        let ring = registry.scrollback(&id).expect("ring");
        assert!(
            !ring.windows(marker.len()).any(|window| window == marker),
            "the ring is supposed to have dropped the older marker"
        );
        let request_bytes = MAX_SCROLLBACK_BYTES as u64 + 128 * 1024;
        let history = registry
            .scrollback_history(&id, request_bytes)
            .expect("history");
        assert!(history.len() > MAX_SCROLLBACK_BYTES, "history did not reach past the ring");
        let text = String::from_utf8_lossy(&history);
        assert!(text.contains("THE FIRST THING IT SAID"), "history lost the older output");
    }
    #[test]
    fn without_a_spool_history_falls_back_to_the_ring() {
        // The in-process app server and every test construct a registry with no
        // disk tier. Asking for history there must answer with what exists
        // rather than nothing.
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Working);
        feed(&registry, &id, b"only in memory
");
        let history = registry.scrollback_history(&id, 1024).expect("history");
        assert_eq!(history, b"only in memory
".to_vec());
    }

    #[test]
    fn history_for_an_unknown_session_is_an_error_not_an_empty_answer() {
        // An empty answer would read as "this session produced nothing".
        let registry = SessionRegistry::new();
        assert!(registry.scrollback_history(&SessionId::new(7), 1024).is_err());
    }

    #[test]
    fn the_store_stops_carrying_output_once_a_log_owns_it() {
        // Persistence rewrites the whole store on a debounce. Copying every
        // session's ring into it once a second duplicated bytes the spool had
        // already appended, and was the most expensive thing it did.
        let dir = spool_scratch("store-free");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Working);
        feed(&registry, &id, b"output
");
        assert_eq!(
            registry.store_snapshot().sessions[0].scrollback,
            b"output
".to_vec(),
            "with no log, the store is the only durable copy"
        );

        registry.set_scrollback_spool(Arc::new(
            ScrollbackSpool::new(&dir.0).expect("spool"),
        ));
        assert!(
            registry.store_snapshot().sessions[0].scrollback.is_empty(),
            "the store is still carrying bytes the log owns"
        );
    }

    #[test]
    fn a_restarted_registry_replays_its_ring_from_the_log() {
        // Because the store no longer carries output, this is the only thing
        // that puts a restored session's last screenful back in front of the
        // operator. Without it, restarting the daemon would blank every pane.
        let dir = spool_scratch("rehydrate");
        let first = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&first, &id, SessionStatus::Working);
        first.set_scrollback_spool(Arc::new(ScrollbackSpool::new(&dir.0).expect("spool")));
        feed(&first, &id, b"said before the restart
");
        first.inner.spool().expect("spool").flush();
        let stored = first.store_snapshot();
        drop(first);

        let restarted = SessionRegistry::from_store(stored);
        assert!(
            restarted.scrollback(&id).expect("ring").is_empty(),
            "nothing should have come from the store"
        );
        restarted.set_scrollback_spool(Arc::new(ScrollbackSpool::new(&dir.0).expect("spool")));
        let ring = restarted.scrollback(&id).expect("ring");
        assert_eq!(ring, b"said before the restart
".to_vec());
        // The grid is replayed too, or a pinned pane restores blank while the
        // focused one has content.
        let grid = restarted.grid_snapshot(&id).expect("grid");
        assert!(
            grid.lines.iter().any(|line| line.contains("before the restart")),
            "the grid was not replayed"
        );
    }

    #[test]
    fn removing_a_session_stops_it_paying_for_disk() {
        let dir = spool_scratch("forget");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Exited);
        registry.set_scrollback_spool(Arc::new(ScrollbackSpool::new(&dir.0).expect("spool")));
        feed(&registry, &id, b"some output
");
        let spool = registry.inner.spool().expect("spool");
        spool.flush();
        assert!(!spool.history(&id, 1024).is_empty());

        registry.archive(&id).expect("archive");
        spool.flush();
        assert!(
            spool.history(&id, 1024).is_empty(),
            "an archived row kept its history on disk"
        );
    }

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

    /// A domain whose sessions record what was written to them.
    struct WritableSession {
        writes: Arc<Mutex<Vec<String>>>,
    }

    impl AgentSession for WritableSession {
        fn write(&self, bytes: &[u8]) -> Result<(), DomainError> {
            self.writes
                .lock()
                .expect("writes")
                .push(String::from_utf8_lossy(bytes).into_owned());
            Ok(())
        }

        fn resize(&self, _size: PtySize) -> Result<(), DomainError> {
            Ok(())
        }

        fn pid(&self) -> Option<u32> {
            Some(1234)
        }

        fn try_wait(&self) -> Result<Option<u32>, DomainError> {
            Ok(None)
        }

        fn wait_for_exit(&self) -> Result<u32, DomainError> {
            Err(DomainError::Message("never exits".into()))
        }

        fn kill(&self) -> Result<(), DomainError> {
            Ok(())
        }
    }

    /// A session with a live, writable pty behind it.
    fn writable_session(
        registry: &SessionRegistry,
        id: &SessionId,
        status: SessionStatus,
        writes: Arc<Mutex<Vec<String>>>,
    ) {
        insert_session(registry, id, status);
        let mut state = lock_state(&registry.inner);
        let entry = state.entries.get_mut(id).expect("entry");
        entry.pty = Some(Arc::new(WritableSession { writes }));
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
