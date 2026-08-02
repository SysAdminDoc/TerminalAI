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
use crate::grid::{TerminalGrid, TerminalGridSnapshot};
use crate::hooks::{HookEvent, HookNotification, HookSignal};
use crate::launch::{LaunchError, LaunchSpec, ResolvedCommand, Resume};
use crate::pty::{PtyError, PtySession, PtySize};
use crate::session::{
    fleet_order, RestartDecision, Session, SessionId, SessionPhase, SessionStatus,
};
use crate::store::{ArchivedSession, SessionStoreSnapshot, StoredSession};

/// Maximum output retained per session in memory. The future daemon can spill
/// older bytes to disk without changing the registry-facing API.
pub const MAX_SCROLLBACK_BYTES: usize = 512 * 1024;

/// Events are deliberately coarse: a view can rebuild its rows from a session
/// update and only the focused pane needs to consume output bytes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RegistryEvent {
    SessionUpdated { session: Session },
    Output { id: SessionId, data: String },
    SessionRemoved { id: SessionId },
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(transparent)]
    Launch(#[from] LaunchError),
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
}

struct State {
    next_id: u64,
    focused: Option<SessionId>,
    entries: BTreeMap<SessionId, Entry>,
    archives: Vec<ArchivedSession>,
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
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    next_id: 1,
                    focused: None,
                    entries: BTreeMap::new(),
                    archives: Vec::new(),
                    subscribers: Vec::new(),
                }),
            }),
        }
    }

    /// Rehydrate rows from the last durable snapshot without starting any
    /// agent automatically. A daemon restart cannot steal a live ConPTY from
    /// the old process, so persisted rows are offered as explicit revives.
    pub fn from_store(snapshot: SessionStoreSnapshot) -> Self {
        let registry = Self::new();
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
            let exit_code = session.last_exit_code;
            session.mark_resurrectable_at(exit_code, SystemTime::now());
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
                },
            );
        }
        drop(state);
        registry
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
        spec: LaunchSpec,
        binary: AgentBinary,
    ) -> Result<SessionId, RegistryError> {
        if spec.agent != binary.agent {
            return Err(RegistryError::AgentMismatch {
                requested: spec.agent.command_name(),
                binary: binary.agent.command_name(),
            });
        }
        let command = spec.resolve(&binary)?;
        let id = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let id = SessionId::new(state.next_id);
            state.next_id = state.next_id.saturating_add(1);
            state.entries.insert(
                id.clone(),
                Entry {
                    session: Session::new(id.clone(), &spec),
                    spec: spec.clone(),
                    command: command.clone(),
                    pty: None,
                    scrollback: RingBuffer::default(),
                    grid: TerminalGrid::default(),
                    generation: 1,
                    stop_requested: false,
                },
            );
            id
        };
        self.emit_session(&id);

        let generation = 1;
        let pty = match self.spawn_pty(&id, &command, generation) {
            Ok(pty) => pty,
            Err(error) => {
                self.remove_entry(&id);
                return Err(error);
            }
        };

        {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            if let Some(entry) = state.entries.get_mut(&id) {
                entry.session.mark_spawned_at(pty.pid(), SystemTime::now());
                entry.pty = Some(pty.clone());
            }
        }
        self.emit_session(&id);
        self.spawn_monitor(id.clone(), pty, generation);
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
        let (command, generation) = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
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
            entry.session.begin_manual_revive_at(SystemTime::now());
            (command, generation)
        };
        self.emit_session(id);

        let pty = match self.spawn_pty(id, &command, generation) {
            Ok(pty) => pty,
            Err(error) => {
                if let Ok(mut state) = self.inner.state.lock() {
                    if let Some(entry) = state.entries.get_mut(id) {
                        if entry.generation == generation {
                            entry.session.mark_resurrectable_at(None, SystemTime::now());
                        }
                    }
                }
                self.emit_session(id);
                return Err(error);
            }
        };

        let accepted = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            match state.entries.get_mut(id) {
                Some(entry)
                    if entry.generation == generation
                        && !entry.stop_requested
                        && entry.pty.is_none() =>
                {
                    entry.session.mark_spawned_at(pty.pid(), SystemTime::now());
                    entry.pty = Some(pty.clone());
                    true
                }
                _ => false,
            }
        };
        if !accepted {
            let _ = pty.kill();
            return Err(RegistryError::NotRunning(id.clone()));
        }
        self.emit_session(id);
        self.spawn_monitor(id.clone(), pty, generation);
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
            if state.focused.as_ref() == Some(id) {
                state.focused = None;
            }
            let archived = ArchivedSession::from_session(&entry.session, &entry.command);
            state.archives.retain(|item| item.id != *id);
            state.archives.push(archived.clone());
            archived
        };
        self.emit(RegistryEvent::SessionRemoved { id: id.clone() });
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
                if entry.session.phase == SessionPhase::Backoff {
                    entry.generation = entry.generation.saturating_add(1);
                    entry.session.mark_resurrectable_at(None, SystemTime::now());
                    drop(state);
                    self.emit_session(id);
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

    /// Apply a normalized Claude/Codex hook to the matching live session.
    ///
    /// A hook may arrive before the agent has written its native id anywhere
    /// else, so SessionStart first falls back to the newest starting session
    /// for the same agent and working directory. Unknown sessions are ignored:
    /// hooks from agents launched outside TerminalAI must not fabricate rows or
    /// delete state.
    pub fn apply_hook(&self, event: HookEvent) -> bool {
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
                                && entry.session.status != SessionStatus::Exited
                        })
                        .map(|(id, _)| id.clone())
                })
        };
        let Some(id) = id else { return false };

        {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let Some(entry) = state.entries.get_mut(&id) else {
                return false;
            };
            if let Some(resume_id) = event.session_id {
                entry.session.resume_id = Some(resume_id);
            }
            match event.signal {
                HookSignal::SessionStart => {}
                HookSignal::Stop => entry.session.set_status(SessionStatus::Idle),
                HookSignal::PreToolUse => entry.session.set_status(SessionStatus::Working),
                HookSignal::PostToolUse => entry.session.set_status(SessionStatus::Thinking),
                HookSignal::Notification { notification } => match notification {
                    HookNotification::PermissionPrompt => {
                        entry.session.set_status(SessionStatus::NeedsApproval)
                    }
                    HookNotification::IdlePrompt => {
                        entry.session.set_status(SessionStatus::AwaitingInput)
                    }
                    HookNotification::Other => {}
                },
            }
        }
        self.emit_session(&id);
        true
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
        let ptys: Vec<_> = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            state
                .entries
                .values_mut()
                .filter_map(|entry| {
                    entry.stop_requested = true;
                    entry.pty.clone()
                })
                .collect()
        };
        for pty in ptys {
            let _ = pty.kill();
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
    ) -> Result<Arc<PtySession>, RegistryError> {
        let weak = Arc::downgrade(&self.inner);
        let callback_id = id.clone();
        let pty = PtySession::spawn(command, crate::pty::default_size(), move |chunk| {
            if let Some(inner) = weak.upgrade() {
                handle_output(&inner, &callback_id, generation, chunk);
            }
        })?;
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
        let restart = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            entry.pty = None;
            entry.generation = entry.generation.saturating_add(1);
            let now = SystemTime::now();
            if entry.stop_requested {
                entry.stop_requested = false;
                entry.session.mark_resurrectable_at(exit_code, now);
                None
            } else {
                match entry.session.schedule_restart_at(exit_code, now) {
                    RestartDecision::Backoff(delay) => Some((entry.generation, delay)),
                    RestartDecision::Failed => None,
                }
            }
        };
        self.emit_session(id);
        if let Some((generation, delay)) = restart {
            self.schedule_restart(id.clone(), generation, delay);
        }
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
            entry.generation = entry.generation.saturating_add(1);
            let generation = entry.generation;
            entry.session.begin_restart_at(SystemTime::now());
            (entry.command.clone(), generation)
        };
        self.emit_session(&id);

        let pty = match self.spawn_pty(&id, &command, generation) {
            Ok(pty) => pty,
            Err(_) => {
                self.restart_spawn_failed(&id, generation);
                return;
            }
        };

        let accepted = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            match state.entries.get_mut(&id) {
                None => false,
                Some(entry)
                    if entry.generation != generation
                        || entry.stop_requested
                        || entry.session.phase != SessionPhase::Starting =>
                {
                    false
                }
                Some(entry) => {
                    entry.session.mark_spawned_at(pty.pid(), SystemTime::now());
                    entry.pty = Some(pty.clone());
                    true
                }
            }
        };
        if !accepted {
            let _ = pty.kill();
            return;
        }
        self.emit_session(&id);
        self.spawn_monitor(id, pty, generation);
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
            match entry.session.schedule_restart_at(None, SystemTime::now()) {
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
                        registry.mark_process_exit(&id, generation, None);
                        break;
                    }
                    Ok(None) => {}
                }
                if !pty.is_running() {
                    registry.mark_process_exit(&id, generation, None);
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            });
    }
}

fn handle_output(inner: &Arc<Inner>, id: &SessionId, generation: u64, bytes: &[u8]) {
    let (send_output, session) = {
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
        if entry.session.status == SessionStatus::Starting {
            entry.session.set_status(SessionStatus::Idle);
        }
        (focused || entry.session.pinned, entry.session.clone())
    };
    if send_output {
        emit_inner(
            inner,
            RegistryEvent::Output {
                id: id.clone(),
                data: String::from_utf8_lossy(bytes).into_owned(),
            },
        );
    }
    emit_inner(inner, RegistryEvent::SessionUpdated { session });
}

fn emit_inner(inner: &Arc<Inner>, event: RegistryEvent) {
    let mut state = inner.state.lock().expect("registry poisoned");
    state
        .subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

fn next_sequence(id: &SessionId) -> u64 {
    id.0.strip_prefix('s')
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1)
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
        handle_output(&registry.inner, &id, 1, b"\r\nfocused");
        assert!(events
            .try_iter()
            .any(|event| matches!(event, RegistryEvent::Output { .. })));
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
    fn hooks_from_other_sessions_are_ignored() {
        let registry = SessionRegistry::new();
        assert!(!registry.apply_hook(HookEvent {
            agent: Agent::Codex,
            session_id: Some("missing".into()),
            cwd: Some(PathBuf::from(".")),
            signal: HookSignal::Stop,
        }));
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
