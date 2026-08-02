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
use std::time::Duration;

use crate::agent::AgentBinary;
use crate::launch::{LaunchError, LaunchSpec};
use crate::pty::{PtyError, PtySession, PtySize};
use crate::session::{fleet_order, Session, SessionId, SessionStatus};

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
}

struct Entry {
    session: Session,
    spec: LaunchSpec,
    pty: Option<Arc<PtySession>>,
    scrollback: RingBuffer,
}

struct State {
    next_id: u64,
    focused: Option<SessionId>,
    entries: BTreeMap<SessionId, Entry>,
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
                    subscribers: Vec::new(),
                }),
            }),
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
                    pty: None,
                    scrollback: RingBuffer::default(),
                },
            );
            id
        };
        self.emit_session(&id);

        let weak = Arc::downgrade(&self.inner);
        let callback_id = id.clone();
        let pty = match PtySession::spawn(&command, crate::pty::default_size(), move |chunk| {
            if let Some(inner) = weak.upgrade() {
                handle_output(&inner, &callback_id, chunk);
            }
        }) {
            Ok(pty) => Arc::new(pty),
            Err(error) => {
                self.remove_entry(&id);
                return Err(error.into());
            }
        };

        {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            if let Some(entry) = state.entries.get_mut(&id) {
                entry.pty = Some(pty.clone());
            }
        }
        self.emit_session(&id);
        self.spawn_monitor(id.clone(), pty);
        Ok(id)
    }

    pub fn write(&self, id: &SessionId, bytes: &[u8]) -> Result<(), RegistryError> {
        let pty = self.pty(id)?;
        pty.write(bytes).map_err(RegistryError::from)
    }

    pub fn resize(&self, id: &SessionId, size: PtySize) -> Result<(), RegistryError> {
        let pty = self.pty(id)?;
        pty.resize(size).map_err(RegistryError::from)
    }

    pub fn kill(&self, id: &SessionId) -> Result<(), RegistryError> {
        let pty = self.pty(id)?;
        pty.kill()?;
        self.mark_exited(id);
        Ok(())
    }

    pub fn mark_read(&self, id: &SessionId) -> Result<(), RegistryError> {
        self.update(id, |session| session.unread = false)
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
            let state = self.inner.state.lock().expect("registry poisoned");
            state
                .entries
                .values()
                .filter_map(|entry| entry.pty.clone())
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

    fn mark_exited(&self, id: &SessionId) {
        let changed = {
            let mut state = self.inner.state.lock().expect("registry poisoned");
            let Some(entry) = state.entries.get_mut(id) else {
                return;
            };
            if entry.session.status == SessionStatus::Exited {
                false
            } else {
                entry.session.set_status(SessionStatus::Exited);
                true
            }
        };
        if changed {
            self.emit_session(id);
        }
    }

    fn spawn_monitor(&self, id: SessionId, pty: Arc<PtySession>) {
        let registry = self.clone();
        let _ = thread::Builder::new()
            .name(format!("terminalai-session-{id}"))
            .spawn(move || loop {
                match pty.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        registry.mark_exited(&id);
                        break;
                    }
                    Ok(None) => {}
                }
                if !pty.is_running() {
                    registry.mark_exited(&id);
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            });
    }
}

fn handle_output(inner: &Arc<Inner>, id: &SessionId, bytes: &[u8]) {
    let data = String::from_utf8_lossy(bytes).into_owned();
    let (event, session) = {
        let mut state = inner.state.lock().expect("registry poisoned");
        let Some(entry) = state.entries.get_mut(id) else {
            return;
        };
        entry.scrollback.push(bytes);
        if let Some(line) = entry.scrollback.last_line() {
            entry.session.set_last_line(&line);
        }
        if entry.session.status == SessionStatus::Starting {
            entry.session.set_status(SessionStatus::Idle);
        }
        (
            RegistryEvent::Output {
                id: id.clone(),
                data,
            },
            entry.session.clone(),
        )
    };
    emit_inner(inner, event);
    emit_inner(inner, RegistryEvent::SessionUpdated { session });
}

fn emit_inner(inner: &Arc<Inner>, event: RegistryEvent) {
    let mut state = inner.state.lock().expect("registry poisoned");
    state
        .subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
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
                    spec,
                    pty: None,
                    scrollback: RingBuffer::default(),
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
}
