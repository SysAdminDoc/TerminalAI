//! Fixtures the registry's tests share.
//!
//! Its own module so the submodules that were split out of `mod.rs` can keep
//! their tests next to the code they exercise. `pub(super)` here means visible
//! throughout `crate::registry` and nowhere else.

#![cfg(test)]

use super::*;
use crate::agent::Agent;
use crate::domain::{AgentDomain, AgentSession, DomainError, OutputHandler};
use crate::launch::spec_for;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub(super) struct RecordingDomain {
    pub(super) spawns: Arc<AtomicUsize>,
    pub(super) commands: Arc<Mutex<Vec<ResolvedCommand>>>,
}

pub(super) struct ExitedSession;

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

pub(super) fn live_entry(
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

/// A row with no process behind it, so archiving is reachable without
/// spawning anything.
pub(super) fn restored_row(id: u64) -> SessionRegistry {
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

pub(super) fn a_landing(files_changed: usize) -> crate::land::Landing {
    crate::land::Landing {
        at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000),
        target: PathBuf::from("C:/repos/project"),
        target_head: "abc1234".into(),
        files_changed,
        verified: Some(true),
    }
}

/// Insert a session in a chosen state without spawning a process.
pub(super) fn insert_session(registry: &SessionRegistry, id: &SessionId, status: SessionStatus) {
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
pub(super) fn apply_test_hook(registry: &SessionRegistry, event: HookEvent) -> bool {
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

pub(super) fn rate_limit_event(resets_in_seconds: u64) -> HookEvent {
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

pub(super) struct SpoolScratch(pub(super) PathBuf);

impl Drop for SpoolScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn spool_scratch(name: &str) -> SpoolScratch {
    let dir = std::env::temp_dir().join(format!(
        "terminalai-registry-spool-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    SpoolScratch(dir)
}

/// Drive output through the same path the pty reader uses.
pub(super) fn feed(registry: &SessionRegistry, id: &SessionId, bytes: &[u8]) {
    let generation = lock_state(&registry.inner)
        .entries
        .get(id)
        .map(|entry| entry.generation)
        .expect("entry");
    handle_output(&registry.inner, id, generation, bytes);
}

/// A domain whose sessions record what was written to them.
pub(super) struct WritableSession {
    pub(super) writes: Arc<Mutex<Vec<String>>>,
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
pub(super) fn writable_session(
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
