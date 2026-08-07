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

mod ingest;
mod lifecycle;
mod output;
mod prompt_queue;
mod provisioning;
mod sampling;
#[cfg(test)]
mod testing;

use lifecycle::restart_scheduler_loop;
use output::{handle_output, spool_forget, RingBuffer};

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
        // A row cannot be both live and archived. Nothing this build writes can
        // produce a store that says otherwise, but a hand-edited file or one
        // written by an older build can, and the two lists are restored
        // independently — so the contradiction is resolved here, at the only
        // boundary where it can arrive, rather than left for whichever code
        // path trips over it first. The live row wins: it is the one with a
        // process history behind it, and archiving it later re-files it with a
        // current timestamp.
        let State {
            archives, entries, ..
        } = &mut *state;
        archives.retain(|archive| !entries.contains_key(&archive.id));
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

    /// Search every session's retained output for a string.
    ///
    /// Reads outside the state lock, deliberately and at some cost in accuracy:
    /// this touches up to `max_bytes` of disk per session across the whole
    /// fleet, and every byte of agent output passes through that same lock, so
    /// holding it here would stall the fleet for the length of a fleet-wide
    /// disk read. The window between snapshotting the rows and reading their
    /// history is real — a session archived in that window reads as empty
    /// rather than as an error, which is the right way for a search to be
    /// wrong.
    ///
    /// Sessions with no match are omitted. Order follows the fleet's own, so
    /// the results read in the same order as the rows behind them.
    pub fn search_scrollback(
        &self,
        query: &crate::search::SearchQuery,
        max_bytes: u64,
    ) -> Vec<crate::search::SessionMatches> {
        let targets: Vec<(SessionId, String)> = {
            let state = lock_state(&self.inner);
            let mut sessions: Vec<_> = state
                .entries
                .values()
                .map(|entry| entry.session.clone())
                .collect();
            sessions.sort_by(fleet_order);
            sessions
                .into_iter()
                .map(|session| (session.id, session.name))
                .collect()
        };
        targets
            .into_iter()
            .filter_map(|(id, name)| {
                let history = self.scrollback_history(&id, max_bytes).ok()?;
                let matches = crate::search::search_output(id, name, &history, query);
                (matches.total_matches > 0).then_some(matches)
            })
            .collect()
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


    fn span_for(&self, id: &SessionId) -> tracing::Span {
        lock_state(&self.inner)
            .entries
            .get(id)
            .map(|entry| entry.span.clone())
            .unwrap_or_else(tracing::Span::none)
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


fn next_sequence(id: &SessionId) -> u64 {
    id.0.strip_prefix('s')
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1)
}

/// What the fleet holds right now, as the admission gate sees it.
///
/// Slots and memory are counted separately because they are released at
/// different moments. `occupies_admission_slot` drops a rate-limited session so
/// it stops blocking the queue — it is running but the provider is refusing it
/// work, so a queued session must not wait behind it. Its process, though, is
/// still there holding its private commit, so it stays in the memory
/// projection: releasing the slot is a statement about progress, not about RAM.
fn admitted_demand(state: &State) -> FleetDemand {
    let mut demand = FleetDemand::default();
    for entry in state.entries.values() {
        let status = entry.session.status;
        if status.occupies_admission_slot() {
            demand.admit(entry.session.agent, entry.session.memory_bytes);
        } else if status.is_live() {
            demand.reside(entry.session.agent, entry.session.memory_bytes);
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



#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentBinary, Origin};
    
    use crate::launch::spec_for;
    use crate::registry::testing::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;


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
    fn a_store_that_says_a_row_is_both_live_and_archived_is_normalised_on_load() {
        // Nothing this build writes can produce such a store — but the two lists
        // are persisted and restored independently, so a hand-edited file or one
        // written by an older build can, and archiving the row would then leave
        // two records of it. Resolved at load, which is the only boundary where
        // the contradiction can arrive.
        let id = SessionId::new(1);
        let cwd = Path::new(".").to_path_buf();
        let spec = spec_for(Agent::Claude, &cwd);
        let session = Session::new(id.clone(), &spec);
        let command = ResolvedCommand {
            program: PathBuf::from("claude.exe"),
            args: Vec::new(),
            cwd: cwd.clone(),
        };
        let registry = SessionRegistry::from_store(SessionStoreSnapshot {
            sessions: vec![StoredSession {
                session: session.clone(),
                spec,
                command: command.clone(),
                scrollback: Vec::new(),
                queue: Default::default(),
            }],
            archives: vec![crate::store::ArchivedSession::from_session(&session, &command)],
            ..SessionStoreSnapshot::default()
        });

        assert_eq!(registry.snapshot().len(), 1, "the live row is kept");
        assert!(
            registry.archives().is_empty(),
            "a row that is live is not also archived"
        );

        // And archiving it now files exactly one record rather than a second.
        registry.archive(&id).expect("archive the restored row");
        let archived: Vec<_> = registry
            .archives()
            .into_iter()
            .map(|archive| archive.id)
            .collect();
        assert_eq!(archived, vec![id]);
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
    fn a_rate_limited_session_keeps_paying_for_its_memory() {
        // The other half of the rule above, and the one that was wrong: a
        // provider refusing a session frees its slot, but the agent process does
        // not exit. Dropping it from the projection let the gate admit work the
        // machine could not physically hold — roughly one whole agent of
        // oversubscription per limited row, which lands the moment the windows
        // reset and every session resumes at once.
        let budget = ASSUMED_SESSION_BYTES_CLAUDE * 2;
        let registry = SessionRegistry::with_admission(
            AdmissionConfig::new(8, None).with_memory_limits(Some(budget), None, None),
        );
        insert_session(&registry, &SessionId::new(1), SessionStatus::Working);
        insert_session(&registry, &SessionId::new(2), SessionStatus::Working);
        let before = registry.admission_snapshot();
        assert_eq!(before.live_sessions, 2);
        assert_eq!(before.admission_block, Some(AdmissionBlock::MemoryBudget));

        assert!(apply_test_hook(&registry, rate_limit_event(1800)));

        let after = registry.admission_snapshot();
        assert_eq!(
            after.live_sessions, 1,
            "the slot is still released — that part was right"
        );
        assert_eq!(
            after.projected_memory_bytes, before.projected_memory_bytes,
            "the process did not exit, so the projection must not shrink"
        );
        assert_eq!(
            after.admission_block,
            Some(AdmissionBlock::MemoryBudget),
            "slots are free, but the memory the fleet holds has not changed"
        );
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
        // Both events name their own instant, so the window is stated rather
        // than waited out. This used to report a zero-second window and sleep
        // 20 ms hoping the wall clock had moved — a coin flip on Windows, whose
        // system clock ticks every 15.6 ms.
        let reported_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let window = Duration::from_secs(300);
        assert!(apply_test_hook_at(
            &registry,
            rate_limit_event(window.as_secs()),
            reported_at
        ));
        assert_eq!(registry.snapshot()[0].status, SessionStatus::RateLimited);

        // What the reset window actually decides is whether the *reading* is
        // still held. The status is not the discriminator: any provider signal
        // moves the row off `RateLimited`, because an agent emitting tool events
        // is by definition not being refused. Asserting on status alone is why
        // this test passed for years without exercising the window at all.
        let still_limited = |at: SystemTime| {
            apply_test_hook_at(
                &registry,
                HookEvent {
                    agent: Agent::Claude,
                    session_id: None,
                    cwd: Some(Path::new(".").to_path_buf()),
                    signal: HookSignal::PostToolUse,
                    progress: None,
                },
                at,
            );
            registry.snapshot()[0].rate_limit.is_some()
        };

        assert!(
            still_limited(reported_at + window - Duration::from_secs(1)),
            "a second short of the reset is not a reset"
        );
        assert!(
            !still_limited(reported_at + window),
            "at the reset the reading stops holding the row"
        );

        assert_ne!(registry.snapshot()[0].status, SessionStatus::RateLimited);
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
}
