//! Rust-owned session lifetime and fleet state.
//!
//! The GUI is intentionally not the owner of a process. A [`SessionRegistry`]
//! keeps the launch specification, live pty, bounded scrollback, focus and
//! fleet metadata together, then publishes small events to any interested
//! shell. This makes closing or reloading a view harmless to live agents.

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::agent::{AgentBinary, Origin};
use crate::app_server::{AgentEvent, AppServerEvent};
use crate::diagnostics::StatusSource;
use crate::environment::{self, EnvironmentError, EnvironmentSpec};
use crate::grid::{TerminalGrid, TerminalGridSnapshot};
use crate::hooks::{HookEvent, HookNotification, HookSignal};
use crate::launch::{LaunchError, LaunchSpec, ResolvedCommand, Resume};
use crate::notification::{NotificationCenter, NotificationChange, NotificationEvent};
use crate::pty::{PtyError, PtySession, PtySize};
use crate::review::{collect_review, ReviewItem};
use crate::session::{
    fleet_order, RestartDecision, Session, SessionId, SessionPhase, SessionStatus,
};
use crate::store::{ArchivedSession, SessionStoreSnapshot, StoredSession};

/// Maximum output retained per session in memory. The future daemon can spill
/// older bytes to disk without changing the registry-facing API.
pub const MAX_SCROLLBACK_BYTES: usize = 512 * 1024;
pub const DEFAULT_MAX_LIVE_SESSIONS: usize = 3;
pub const DEFAULT_SESSION_BUDGET_USD: f64 = 5.0;

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

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdmissionSnapshot {
    pub max_live_sessions: usize,
    pub live_sessions: usize,
    pub queued_sessions: usize,
    pub aggregate_cost_usd: f64,
}

/// Events are deliberately coarse: a view can rebuild its rows from a session
/// update and only the focused pane needs to consume output bytes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RegistryEvent {
    SessionUpdated { session: Session },
    Notification { event: NotificationEvent },
    AgentEvent { event: AgentEvent },
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
    #[error("session is no longer running: {0}")]
    NotRunning(SessionId),
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
}

struct State {
    next_id: u64,
    focused: Option<SessionId>,
    entries: BTreeMap<SessionId, Entry>,
    archives: Vec<ArchivedSession>,
    queue: VecDeque<SessionId>,
    admission: AdmissionConfig,
    notifications: NotificationCenter,
    subscribers: Vec<Sender<RegistryEvent>>,
}

struct Inner {
    state: Mutex<State>,
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
    pub fn new() -> Self {
        Self::with_admission(AdmissionConfig::default())
    }

    pub fn with_admission(admission: AdmissionConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
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
            }),
        }
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
        let mut state = registry.inner.state.lock().expect("registry poisoned");
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
                    teardown_done: true,
                },
            );
        }
        drop(state);
        registry
    }

    pub fn admission_snapshot(&self) -> AdmissionSnapshot {
        let state = self.inner.state.lock().expect("registry poisoned");
        AdmissionSnapshot {
            max_live_sessions: state.admission.max_live_sessions,
            live_sessions: admitted_count(&state),
            queued_sessions: state.queue.len(),
            aggregate_cost_usd: state
                .entries
                .values()
                .filter_map(|entry| entry.session.cost_usd)
                .sum(),
        }
    }

    pub fn is_queued(&self, id: &SessionId) -> Result<bool, RegistryError> {
        let state = self.inner.state.lock().expect("registry poisoned");
        state
            .entries
            .get(id)
            .map(|entry| entry.session.status == SessionStatus::Queued)
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    /// Capture only serializable state; live PTY handles and parsed grids are
    /// intentionally reconstructed on restore.
    pub fn store_snapshot(&self) -> SessionStoreSnapshot {
        let state = self.inner.state.lock().expect("registry poisoned");
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
        let (sender, receiver) = mpsc::channel();
        self.inner
            .state
            .lock()
            .expect("registry poisoned")
            .subscribers
            .push(sender);
        receiver
    }

    /// Current rows, sorted with attention first and longest dwell time next.
    pub fn snapshot(&self) -> Vec<Session> {
        let state = self.inner.state.lock().expect("registry poisoned");
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
            let state = self.inner.state.lock().expect("registry poisoned");
            state
                .entries
                .values()
                .map(|entry| entry.session.clone())
                .collect()
        };
        let mut reviews: Vec<_> = sessions
            .iter()
            .map(collect_review)
            .filter(|item| item.files_changed > 0 || item.error.is_some())
            .collect();
        reviews.sort_by(|a, b| {
            b.review_cost
                .cmp(&a.review_cost)
                .then_with(|| b.conflicts.len().cmp(&a.conflicts.len()))
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        reviews
    }

    pub fn focused(&self) -> Option<SessionId> {
        self.inner
            .state
            .lock()
            .expect("registry poisoned")
            .focused
            .clone()
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
        let admission = self
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .admission;
        if spec.agent == crate::agent::Agent::Claude && spec.max_budget_usd.is_none() {
            spec.max_budget_usd = admission.default_budget_usd;
        }
        let command = spec.resolve(&binary)?;
        let (id, queued) = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let id = SessionId::new(state.next_id);
            state.next_id = state.next_id.saturating_add(1);
            let queued = admitted_count(&state) >= state.admission.max_live_sessions;
            let mut session = Session::new(id.clone(), &spec);
            if queued {
                session.mark_queued_at(SystemTime::now());
                state.queue.push_back(id.clone());
            }
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
                    teardown_done: true,
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
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let admission_full = admitted_count(&state) >= state.admission.max_live_sessions;
            let entry = state
                .entries
                .get_mut(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?;
            if entry.pty.is_some() {
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
            }
            (command, generation, queued)
        };
        self.emit_session(id);
        if queued {
            return Ok(id.clone());
        }

        if let Err(error) = self.start_entry(id, command, generation) {
            if let Ok(mut state) = self.inner.state.lock() {
                if let Some(entry) = state.entries.get_mut(id) {
                    if entry.generation == generation {
                        entry.session.mark_resurrectable_at_from(
                            None,
                            SystemTime::now(),
                            StatusSource::Manual,
                        );
                    }
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
        let archived = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
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
            archived
        };
        self.emit(RegistryEvent::SessionRemoved { id: id.clone() });
        self.drain_queue();
        Ok(archived)
    }

    pub fn resize(&self, id: &SessionId, size: PtySize) -> Result<(), RegistryError> {
        let pty = self.pty(id)?;
        pty.resize(size).map_err(RegistryError::from)
    }

    pub fn kill(&self, id: &SessionId) -> Result<(), RegistryError> {
        let (pty, generation) = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let entry = state
                .entries
                .get_mut(id)
                .ok_or_else(|| RegistryError::Missing(id.clone()))?;
            entry.stop_requested = true;
            let Some(pty) = entry.pty.clone() else {
                if entry.session.status == SessionStatus::Queued {
                    entry.generation = entry.generation.saturating_add(1);
                    let now = SystemTime::now();
                    entry
                        .session
                        .mark_resurrectable_at_from(None, now, StatusSource::Manual);
                    let session = entry.session.clone();
                    state.queue.retain(|queued| queued != id);
                    drop(state);
                    self.emit(RegistryEvent::SessionUpdated { session });
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
                    let notification = state.notifications.observe(
                        &session,
                        previous_status,
                        previous_state_since,
                        now,
                    );
                    drop(state);
                    self.emit(RegistryEvent::SessionUpdated { session });
                    self.emit_notification_change(notification);
                    self.drain_queue();
                    return Ok(());
                }
                return Err(RegistryError::Missing(id.clone()));
            };
            (pty, entry.generation)
        };
        if let Err(error) = pty.kill() {
            if let Ok(mut state) = self.inner.state.lock() {
                if let Some(entry) = state.entries.get_mut(id) {
                    entry.stop_requested = false;
                }
            }
            return Err(error.into());
        }
        self.mark_process_exit(id, generation, None);
        Ok(())
    }

    pub fn mark_read(&self, id: &SessionId) -> Result<(), RegistryError> {
        self.update(id, |session| session.unread = false)
    }

    pub fn mark_reviewed(&self, id: &SessionId) -> Result<(), RegistryError> {
        self.update(id, |session| session.reviewed = true)
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

    fn apply_hook_from(&self, event: HookEvent, source: StatusSource) -> bool {
        let id = {
            let state = self.inner.state.lock().expect("registry poisoned");
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

        let (session, notification) = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let Some(entry) = state.entries.get_mut(&id) else {
                return false;
            };
            let previous_status = entry.session.status;
            let previous_state_since = entry.session.state_since;
            if let Some(resume_id) = event.session_id {
                entry.session.resume_id = Some(resume_id);
            }
            match event.signal {
                HookSignal::SessionStart => {}
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
            let notification = state.notifications.observe(
                &session,
                previous_status,
                previous_state_since,
                SystemTime::now(),
            );
            (session, notification)
        };
        self.emit(RegistryEvent::SessionUpdated { session });
        self.emit_notification_change(notification);
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
            },
            StatusSource::AppServer,
        )
    }

    fn has_native_session(&self, agent: crate::agent::Agent, native_id: &str) -> bool {
        let state = self.inner.state.lock().expect("registry poisoned");
        state.entries.values().any(|entry| {
            entry.session.agent == agent && entry.session.resume_id.as_deref() == Some(native_id)
        })
    }

    pub fn focus(&self, id: Option<SessionId>) -> Result<(), RegistryError> {
        if let Some(id) = &id {
            self.require(id)?;
        }
        {
            let mut state = self.inner.state.lock().expect("registry poisoned");
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
            let mut state = self.inner.state.lock().expect("registry poisoned");
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
        let state = self.inner.state.lock().expect("registry poisoned");
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
        let state = self.inner.state.lock().expect("registry poisoned");
        state
            .entries
            .get(id)
            .map(|entry| entry.grid.snapshot())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    pub fn spec(&self, id: &SessionId) -> Result<LaunchSpec, RegistryError> {
        let state = self.inner.state.lock().expect("registry poisoned");
        state
            .entries
            .get(id)
            .map(|entry| entry.spec.clone())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    pub fn shutdown(&self) {
        let sessions: Vec<_> = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            state
                .entries
                .values_mut()
                .filter_map(|entry| {
                    entry.stop_requested = true;
                    let pty = entry.pty.clone()?;
                    let teardown = if entry.teardown_done {
                        None
                    } else {
                        entry.teardown_done = true;
                        Some((
                            entry.session.cwd.clone(),
                            entry.spec.environment.clone(),
                            entry.session.ports.clone(),
                        ))
                    };
                    Some((entry.session.id.clone(), pty, teardown))
                })
                .collect()
        };
        for (id, pty, teardown) in sessions {
            let _ = pty.kill();
            if let Some((cwd, spec, ports)) = teardown {
                let _ = environment::run_teardown(&spec, &id.0, &cwd, &ports);
            }
        }
    }

    fn pty(&self, id: &SessionId) -> Result<Arc<PtySession>, RegistryError> {
        let state = self.inner.state.lock().expect("registry poisoned");
        state
            .entries
            .get(id)
            .and_then(|entry| entry.pty.clone())
            .ok_or_else(|| RegistryError::Missing(id.clone()))
    }

    fn require(&self, id: &SessionId) -> Result<(), RegistryError> {
        let state = self.inner.state.lock().expect("registry poisoned");
        if state.entries.contains_key(id) {
            Ok(())
        } else {
            Err(RegistryError::Missing(id.clone()))
        }
    }

    fn update(&self, id: &SessionId, f: impl FnOnce(&mut Session)) -> Result<(), RegistryError> {
        {
            let mut state = self.inner.state.lock().expect("registry poisoned");
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
        let pty = PtySession::spawn_with_environment(
            command,
            crate::pty::default_size(),
            environment,
            move |chunk| {
                if let Some(inner) = weak.upgrade() {
                    handle_output(&inner, &callback_id, generation, chunk);
                }
            },
        )?;
        Ok(Arc::new(pty))
    }

    fn emit_session(&self, id: &SessionId) {
        let session = {
            let state = self.inner.state.lock().expect("registry poisoned");
            state.entries.get(id).map(|entry| entry.session.clone())
        };
        if let Some(session) = session {
            self.emit(RegistryEvent::SessionUpdated { session });
        }
    }

    fn emit_notification_change(&self, change: Option<NotificationChange>) {
        if let Some(event) = change.and_then(NotificationChange::into_event) {
            self.emit(RegistryEvent::Notification { event });
        }
    }

    fn start_entry(
        &self,
        id: &SessionId,
        command: ResolvedCommand,
        generation: u64,
    ) -> Result<(), RegistryError> {
        let (cwd, environment_spec, ports) = self.runtime_environment(id)?;
        let environment = environment::variables(&id.0, &ports);
        environment::run_setup(&environment_spec, &id.0, &cwd, &ports)?;
        let pty = match self.spawn_pty(id, &command, generation, &environment) {
            Ok(pty) => pty,
            Err(error) => {
                let _ = environment::run_teardown(&environment_spec, &id.0, &cwd, &ports);
                return Err(error);
            }
        };
        let accepted = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
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
        self.emit_session(id);
        self.spawn_monitor(id.clone(), pty, generation);
        Ok(())
    }

    fn runtime_environment(
        &self,
        id: &SessionId,
    ) -> Result<(std::path::PathBuf, EnvironmentSpec, Vec<u16>), RegistryError> {
        let state = self.inner.state.lock().expect("registry poisoned");
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
                let mut state = self.inner.state.lock().expect("registry poisoned");
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
                (id, entry.command.clone(), entry.generation)
            };

            self.emit_session(&id);
            if self.start_entry(&id, command, generation).is_err() {
                let session = {
                    let mut state = self.inner.state.lock().expect("registry poisoned");
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
        let mut state = self.inner.state.lock().expect("registry poisoned");
        state
            .subscribers
            .retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    fn remove_entry(&self, id: &SessionId) {
        let removed = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            if state.focused.as_ref() == Some(id) {
                state.focused = None;
            }
            state.entries.remove(id).is_some()
        };
        if removed {
            self.emit(RegistryEvent::SessionRemoved { id: id.clone() });
        }
    }

    fn mark_process_exit(&self, id: &SessionId, generation: u64, exit_code: Option<u32>) {
        let (restart, session, notification, teardown) = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            let previous_status = entry.session.status;
            let previous_state_since = entry.session.state_since;
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
            let session = entry.session.clone();
            let teardown = if entry.teardown_done {
                None
            } else {
                entry.teardown_done = true;
                Some((
                    entry.session.cwd.clone(),
                    entry.spec.environment.clone(),
                    entry.session.ports.clone(),
                ))
            };
            let notification =
                state
                    .notifications
                    .observe(&session, previous_status, previous_state_since, now);
            (restart, session, notification, teardown)
        };
        if let Some((cwd, spec, ports)) = teardown {
            let _ = environment::run_teardown(&spec, &id.0, &cwd, &ports);
        }
        self.emit(RegistryEvent::SessionUpdated { session });
        self.emit_notification_change(notification);
        self.drain_queue();
        if let Some((generation, delay)) = restart {
            self.schedule_restart(id.clone(), generation, delay);
        }
    }

    fn mark_process_unknown(&self, id: &SessionId, generation: u64) {
        let session = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            if entry.session.status == SessionStatus::Unknown {
                return;
            }
            entry.session.mark_unknown_at(SystemTime::now());
            entry.session.clone()
        };
        self.emit(RegistryEvent::SessionUpdated { session });
    }

    fn schedule_restart(&self, id: SessionId, generation: u64, delay: Duration) {
        let registry = self.clone();
        let _ = thread::Builder::new()
            .name(format!("terminalai-restart-{id}"))
            .spawn(move || {
                thread::sleep(delay);
                registry.restart(id, generation);
            });
    }

    fn restart(&self, id: SessionId, pending_generation: u64) {
        let (command, generation) = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
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
                self.schedule_restart(id, pending_generation, Duration::from_millis(250));
                return;
            }
            entry.generation = entry.generation.saturating_add(1);
            let generation = entry.generation;
            entry.session.begin_restart_at(SystemTime::now());
            (entry.command.clone(), generation)
        };
        self.emit_session(&id);
        if self.start_entry(&id, command, generation).is_err() {
            self.restart_spawn_failed(&id, generation);
        }
    }

    fn restart_spawn_failed(&self, id: &SessionId, generation: u64) {
        let restart = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            entry.generation = entry.generation.saturating_add(1);
            match entry.session.schedule_restart_at_from(
                None,
                SystemTime::now(),
                StatusSource::Supervisor,
            ) {
                RestartDecision::Backoff(delay) => Some((entry.generation, delay)),
                RestartDecision::Failed => None,
            }
        };
        self.emit_session(id);
        if let Some((generation, delay)) = restart {
            self.schedule_restart(id.clone(), generation, delay);
        }
    }

    fn spawn_monitor(&self, id: SessionId, pty: Arc<PtySession>, generation: u64) {
        let registry = self.clone();
        let _ = thread::Builder::new()
            .name(format!("terminalai-session-{id}"))
            .spawn(move || loop {
                match pty.try_wait() {
                    Ok(Some(status)) => {
                        registry.mark_process_exit(&id, generation, Some(status));
                        break;
                    }
                    Err(_) => {
                        registry.mark_process_unknown(&id, generation);
                        thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    Ok(None) => {}
                }
                thread::sleep(Duration::from_millis(50));
            });
    }
}

fn handle_output(inner: &Arc<Inner>, id: &SessionId, generation: u64, bytes: &[u8]) {
    let (send_output, session, notification) = {
        let mut state = inner.state.lock().expect("registry poisoned");
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
        let notification = (previous_status != session.status).then(|| {
            state.notifications.observe(
                &session,
                previous_status,
                previous_state_since,
                SystemTime::now(),
            )
        });
        (focused || session.pinned, session, notification.flatten())
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
    if let Some(event) = notification.and_then(NotificationChange::into_event) {
        emit_inner(inner, RegistryEvent::Notification { event });
    }
}

fn emit_inner(inner: &Arc<Inner>, event: RegistryEvent) {
    let mut state = inner.state.lock().expect("registry poisoned");
    state
        .subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
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
        let bytes = self.to_vec();
        let text = String::from_utf8_lossy(&bytes);
        text.split(['\r', '\n'])
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::to_owned)
    }
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
    fn admission_queues_overflow_and_applies_default_budget() {
        let registry = SessionRegistry::with_admission(AdmissionConfig::new(1, Some(4.5)));
        let cwd = Path::new(".").to_path_buf();
        let active_id = SessionId::new(99);
        let spec = spec_for(Agent::Claude, &cwd);
        let mut active = Session::new(active_id.clone(), &spec);
        active.status = SessionStatus::Idle;
        active.phase = SessionPhase::Idle;
        active.health = crate::session::SessionHealth::Degraded;
        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .insert(
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
            registry
                .inner
                .state
                .lock()
                .expect("registry poisoned")
                .entries[&queued_id]
                .spec
                .max_budget_usd,
            Some(4.5)
        );

        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .remove(&active_id);
        registry.drain_queue();
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
        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .insert(
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
        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .insert(
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
        let session = Session::new(id.clone(), &spec);
        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .insert(
                id.clone(),
                Entry {
                    session,
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
                    teardown_done: true,
                },
            );

        let archive = registry.archive(&id).expect("archive");
        assert_eq!(archive.id, id);
        assert!(registry.snapshot().is_empty());
        let stored = registry.store_snapshot();
        assert!(stored.sessions.is_empty());
        assert_eq!(stored.archives.len(), 1);
        assert_eq!(stored.archives[0].command, "claude.exe --resume native-1");
    }

    #[test]
    fn snapshots_are_sorted_by_attention() {
        let registry = SessionRegistry::new();
        let mut state = registry.inner.state.lock().expect("registry poisoned");
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
                    teardown_done: true,
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
        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .insert(
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
                },
            );

        assert!(registry.apply_hook(HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd.clone()),
            signal: HookSignal::SessionStart,
        }));
        assert!(registry.apply_hook(HookEvent {
            agent: Agent::Claude,
            session_id: Some("native-1".into()),
            cwd: Some(cwd.clone()),
            signal: HookSignal::Notification {
                notification: HookNotification::PermissionPrompt,
            },
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
        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .insert(
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
        registry
            .inner
            .state
            .lock()
            .expect("registry poisoned")
            .entries
            .insert(
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
}
