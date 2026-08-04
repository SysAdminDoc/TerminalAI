//! Versioned, human-readable session persistence.
//!
//! The store contains enough state to restore rows and offer an explicit
//! native resume. It never contains a live PTY handle. Raw scrollback is kept
//! bounded by the registry before it reaches this type, so a daemon restart
//! can replay the same tail into a newly attached renderer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::agent::Agent;
use crate::atomic_file::write_atomic;
use crate::launch::{LaunchSpec, ResolvedCommand};
use crate::session::{Session, SessionId};

pub const SESSION_STORE_MAGIC: &str = "TerminalAI.session-store";
pub const SESSION_STORE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStoreSnapshot {
    #[serde(default)]
    pub magic: String,
    pub schema_version: u32,
    pub sessions: Vec<StoredSession>,
    pub archives: Vec<ArchivedSession>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for SessionStoreSnapshot {
    fn default() -> Self {
        Self {
            magic: SESSION_STORE_MAGIC.to_owned(),
            schema_version: SESSION_STORE_SCHEMA_VERSION,
            sessions: Vec::new(),
            archives: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredSession {
    pub session: Session,
    pub spec: LaunchSpec,
    pub command: ResolvedCommand,
    /// Loaded from `<store>.scrollback/<session>.bin`; old inline stores are
    /// still accepted for a one-time compatibility read.
    #[serde(default, skip_serializing)]
    pub scrollback: Vec<u8>,
    /// Prompts still waiting their turn. Persisted because a daemon restart
    /// that restores a session should restore what it was queued to do next —
    /// retyping them is the one thing the queue exists to avoid.
    #[serde(default)]
    pub queue: crate::queue::PromptQueue,
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
    #[error("session store magic {found:?} is not recognized")]
    InvalidMagic { found: String },
}

impl SessionStoreSnapshot {
    pub fn read(path: &Path) -> Result<Option<Self>, SessionStoreError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut snapshot: Self = serde_json::from_str(&text)?;
        if !snapshot.magic.is_empty() && snapshot.magic != SESSION_STORE_MAGIC {
            return Err(SessionStoreError::InvalidMagic {
                found: snapshot.magic,
            });
        }
        if snapshot.schema_version > SESSION_STORE_SCHEMA_VERSION {
            return Err(SessionStoreError::UnsupportedVersion {
                found: snapshot.schema_version,
                supported: SESSION_STORE_SCHEMA_VERSION,
            });
        }
        load_sidecars(path, &mut snapshot)?;
        if snapshot.schema_version < SESSION_STORE_SCHEMA_VERSION || snapshot.magic.is_empty() {
            let old_version = snapshot.schema_version;
            backup_legacy(path, old_version)?;
            snapshot.magic = SESSION_STORE_MAGIC.to_owned();
            snapshot.schema_version = SESSION_STORE_SCHEMA_VERSION;
            snapshot.write(path)?;
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
        if self.magic != SESSION_STORE_MAGIC {
            return Err(SessionStoreError::InvalidMagic {
                found: self.magic.clone(),
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        write_sidecars(path, self)?;
        write_atomic(path, text.as_bytes(), true)?;
        Ok(())
    }
}

fn backup_legacy(path: &Path, version: u32) -> Result<PathBuf, std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("sessions");
    let mut candidate = parent.join(format!("{stem}.v{version}.bak"));
    for suffix in 1..1000 {
        if !candidate.exists() {
            fs::copy(path, &candidate)?;
            return Ok(candidate);
        }
        candidate = parent.join(format!("{stem}.v{version}.{suffix}.bak"));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not choose a unique session-store backup path",
    ))
}

fn sidecar_dir(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sessions.json");
    path.with_file_name(format!("{name}.scrollback"))
}

fn sidecar_path(path: &Path, id: &SessionId) -> PathBuf {
    sidecar_dir(path).join(format!("{}.bin", sidecar_name(id)))
}

fn sidecar_name(id: &SessionId) -> String {
    let mut name = String::with_capacity(id.0.len());
    for byte in id.0.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            name.push(byte as char);
        } else {
            name.push_str(&format!("_{byte:02x}"));
        }
    }
    if name.is_empty() {
        "session".into()
    } else {
        name
    }
}

fn load_sidecars(
    path: &Path,
    snapshot: &mut SessionStoreSnapshot,
) -> Result<(), SessionStoreError> {
    for stored in &mut snapshot.sessions {
        let sidecar = sidecar_path(path, &stored.session.id);
        match fs::read(sidecar) {
            Ok(bytes) => stored.scrollback = bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_sidecars(path: &Path, snapshot: &SessionStoreSnapshot) -> Result<(), SessionStoreError> {
    let directory = sidecar_dir(path);
    if snapshot
        .sessions
        .iter()
        .any(|session| !session.scrollback.is_empty())
    {
        fs::create_dir_all(&directory)?;
    }
    for stored in &snapshot.sessions {
        let sidecar = sidecar_path(path, &stored.session.id);
        if stored.scrollback.is_empty() {
            match fs::remove_file(sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            write_atomic(&sidecar, &stored.scrollback, false)?;
        }
    }
    Ok(())
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
            magic: SESSION_STORE_MAGIC.to_owned(),
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
                queue: Default::default(),
            }],
            archives: Vec::new(),
            extra: BTreeMap::from([("future_field".into(), serde_json::json!({"retained": true}))]),
        };
        snapshot.write(&path).expect("write");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("\n  \"magic\": \"TerminalAI.session-store\","));
        assert!(text.contains("\n  \"schema_version\": 2,"));
        assert!(!text.contains("\"scrollback\""));
        assert!(text.contains("\"future_field\""));
        assert!(sidecar_path(&path, &SessionId::new(1)).is_file());
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
    fn legacy_schema_is_migrated_and_unknown_fields_survive() {
        let dir = test_dir();
        let path = dir.join("sessions.json");
        let legacy =
            r#"{"schema_version":1,"sessions":[],"archives":[],"future_field":{"retained":true}}"#;
        fs::write(&path, legacy).expect("seed legacy store");

        let snapshot = SessionStoreSnapshot::read(&path)
            .expect("migrate legacy store")
            .expect("snapshot");
        assert_eq!(snapshot.magic, SESSION_STORE_MAGIC);
        assert_eq!(snapshot.schema_version, SESSION_STORE_SCHEMA_VERSION);
        assert_eq!(snapshot.extra["future_field"]["retained"], true);
        assert_eq!(
            fs::read_to_string(dir.join("sessions.v1.bak")).expect("backup"),
            legacy
        );
        let migrated = fs::read_to_string(&path).expect("migrated store");
        assert!(migrated.contains("\"magic\": \"TerminalAI.session-store\""));
        assert!(migrated.contains("\"future_field\""));
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
