//! Versioned, human-readable session persistence.
//!
//! The store contains enough state to restore rows and offer an explicit
//! native resume. It never contains a live PTY handle. Raw scrollback is kept
//! bounded by the registry before it reaches this type, so a daemon restart
//! can replay the same tail into a newly attached renderer.

use std::fs;
use std::path::{Path, PathBuf};

use crate::agent::Agent;
use crate::atomic_file::write_atomic;
use crate::launch::{LaunchSpec, ResolvedCommand};
use crate::session::{Session, SessionId};

pub const SESSION_STORE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStoreSnapshot {
    pub schema_version: u32,
    pub sessions: Vec<StoredSession>,
    pub archives: Vec<ArchivedSession>,
}

impl Default for SessionStoreSnapshot {
    fn default() -> Self {
        Self {
            schema_version: SESSION_STORE_SCHEMA_VERSION,
            sessions: Vec::new(),
            archives: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredSession {
    pub session: Session,
    pub spec: LaunchSpec,
    pub command: ResolvedCommand,
    pub scrollback: Vec<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchivedSession {
    pub id: SessionId,
    pub agent: Agent,
    pub name: String,
    pub cwd: PathBuf,
    pub command: String,
}

impl ArchivedSession {
    pub fn from_session(session: &Session, command: &ResolvedCommand) -> Self {
        Self {
            id: session.id.clone(),
            agent: session.agent,
            name: session.name.clone(),
            cwd: session.cwd.clone(),
            command: command.preview(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("session store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("session store JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session store schema {found} is not supported (current schema: {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
}

impl SessionStoreSnapshot {
    pub fn read(path: &Path) -> Result<Option<Self>, SessionStoreError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let snapshot: Self = serde_json::from_str(&text)?;
        if snapshot.schema_version != SESSION_STORE_SCHEMA_VERSION {
            return Err(SessionStoreError::UnsupportedVersion {
                found: snapshot.schema_version,
                supported: SESSION_STORE_SCHEMA_VERSION,
            });
        }
        Ok(Some(snapshot))
    }

    pub fn write(&self, path: &Path) -> Result<(), SessionStoreError> {
        if self.schema_version != SESSION_STORE_SCHEMA_VERSION {
            return Err(SessionStoreError::UnsupportedVersion {
                found: self.schema_version,
                supported: SESSION_STORE_SCHEMA_VERSION,
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        write_atomic(path, text.as_bytes(), true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::spec_for;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-session-store-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    #[test]
    fn round_trip_is_pretty_json_and_keeps_replay_bytes() {
        let dir = test_dir();
        let path = dir.join("sessions.json");
        let cwd = std::env::current_dir().expect("cwd");
        let spec = spec_for(Agent::Claude, &cwd);
        let session = Session::new(SessionId::new(1), &spec);
        let snapshot = SessionStoreSnapshot {
            schema_version: SESSION_STORE_SCHEMA_VERSION,
            sessions: vec![StoredSession {
                session,
                spec,
                command: ResolvedCommand {
                    program: PathBuf::from("claude.exe"),
                    args: vec!["--resume".into(), "native-1".into()],
                    cwd,
                },
                scrollback: b"hello\x1b[2J".to_vec(),
            }],
            archives: Vec::new(),
        };
        snapshot.write(&path).expect("write");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("\n  \"schema_version\": 1,"));
        assert_eq!(
            SessionStoreSnapshot::read(&path)
                .expect("read snapshot")
                .expect("snapshot")
                .sessions[0]
                .scrollback,
            b"hello\x1b[2J"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn newer_schema_is_refused_before_restore() {
        let dir = test_dir();
        let path = dir.join("sessions.json");
        fs::write(
            &path,
            r#"{"schema_version":99,"sessions":[],"archives":[]}"#,
        )
        .expect("seed store");
        assert!(matches!(
            SessionStoreSnapshot::read(&path),
            Err(SessionStoreError::UnsupportedVersion { found: 99, .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn archive_contains_layout_cwd_and_command_only() {
        let cwd = std::env::current_dir().expect("cwd");
        let spec = spec_for(Agent::Claude, &cwd);
        let session = Session::new(SessionId::new(1), &spec);
        let command = ResolvedCommand {
            program: PathBuf::from("claude.exe"),
            args: vec!["--model".into(), "opus".into()],
            cwd: cwd.clone(),
        };
        let archive = ArchivedSession::from_session(&session, &command);
        assert_eq!(archive.cwd, cwd);
        assert_eq!(archive.command, "claude.exe --model opus");
        assert_eq!(archive.name, session.name);
    }
}
