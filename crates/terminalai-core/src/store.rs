//! Versioned, human-readable session persistence.
//!
//! The store contains enough state to restore rows and offer an explicit
//! native resume. It never contains a live PTY handle. Raw scrollback is kept
//! bounded by the registry before it reaches this type, so a daemon restart
//! can replay the same tail into a newly attached renderer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::agent::Agent;
use crate::atomic_file::write_atomic;
use crate::launch::{LaunchSpec, ResolvedCommand};
use crate::session::{Session, SessionId};

pub const SESSION_STORE_MAGIC: &str = "TerminalAI.session-store";
pub const SESSION_STORE_SCHEMA_VERSION: u32 = 2;

/// How many archived sessions are kept.
///
/// The archive list is serialized into every full store snapshot, and the store
/// is rewritten after a 200 ms quiet period and at least once per second under
/// sustained output — so an unbounded list makes persistence cost rise for the
/// life of the install, on the hot path. Bounded here rather than in a sidecar
/// file: 200 records of id, name, folder and command is tens of kilobytes, which
/// is small beside the session rows already in the same document.
pub const MAX_ARCHIVES: usize = 200;

/// How long an archived session is kept regardless of how few there are.
///
/// A count bound alone keeps a row from a machine's first week alive forever on
/// a lightly-used install; an age bound alone lets a busy day grow without
/// limit. Both apply.
pub const ARCHIVE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStoreSnapshot {
    #[serde(default)]
    pub magic: String,
    pub schema_version: u32,
    pub sessions: Vec<StoredSession>,
    pub archives: Vec<ArchivedSession>,
    /// Fleet spend buckets behind the admission ceiling. Defaulted so a store
    /// written before the ceiling existed still loads, and persisted so the
    /// ceiling cannot be reset by restarting the daemon.
    #[serde(default)]
    pub spend: Vec<crate::spend::SpendBucket>,
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
            spend: Vec::new(),
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
    /// When the row was archived. Optional because stores written before the
    /// bound existed have no stamp, and an absent stamp must not read as
    /// "archived at the epoch" — that would delete every pre-upgrade record on
    /// first load. Unstamped records age out through the count bound instead.
    #[serde(default)]
    pub archived_at: Option<SystemTime>,
}

impl ArchivedSession {
    pub fn from_session(session: &Session, command: &ResolvedCommand) -> Self {
        Self::from_session_at(session, command, SystemTime::now())
    }

    pub fn from_session_at(session: &Session, command: &ResolvedCommand, at: SystemTime) -> Self {
        Self {
            id: session.id.clone(),
            agent: session.agent,
            name: session.name.clone(),
            cwd: session.cwd.clone(),
            command: command.preview(),
            archived_at: Some(at),
        }
    }
}

/// Drop archived sessions past either bound, oldest first.
///
/// The list is append-ordered, so the front is the oldest and draining from it
/// is what the count bound wants. A stamp in the future (the clock moved back)
/// keeps the record rather than deleting it: losing history to a clock change is
/// worse than carrying one record too long.
pub fn trim_archives(archives: &mut Vec<ArchivedSession>, now: SystemTime) {
    archives.retain(|archive| match archive.archived_at {
        Some(at) => now
            .duration_since(at)
            .map(|age| age <= ARCHIVE_MAX_AGE)
            .unwrap_or(true),
        None => true,
    });
    let excess = archives.len().saturating_sub(MAX_ARCHIVES);
    if excess > 0 {
        archives.drain(..excess);
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
        if byte.is_ascii_alphanumeric() || byte == b'-' {
            name.push(byte as char);
        } else {
            name.push_str(&format!("_{byte:02x}"));
        }
    }
    if name.is_empty() {
        "_".into()
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
        match fs::read(&sidecar) {
            Ok(bytes) => stored.scrollback = bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                stored.scrollback.clear();
                tracing::warn!(
                    session = %stored.session.id,
                    sidecar = %sidecar.display(),
                    %error,
                    "could not read persisted scrollback; restoring without history"
                );
            }
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
            spend: Vec::new(),
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
    fn an_unreadable_sidecar_does_not_discard_the_other_sessions() {
        let dir = test_dir();
        let path = dir.join("sessions.json");
        let cwd = std::env::current_dir().expect("cwd");
        let spec = spec_for(Agent::Claude, &cwd);
        let command = ResolvedCommand {
            program: PathBuf::from("claude.exe"),
            args: vec!["--resume".into(), "native-1".into()],
            cwd: cwd.clone(),
        };
        let snapshot = SessionStoreSnapshot {
            magic: SESSION_STORE_MAGIC.to_owned(),
            schema_version: SESSION_STORE_SCHEMA_VERSION,
            spend: Vec::new(),
            sessions: vec![
                StoredSession {
                    session: Session::new(SessionId::new(1), &spec),
                    spec: spec.clone(),
                    command: command.clone(),
                    scrollback: b"first".to_vec(),
                    queue: Default::default(),
                },
                StoredSession {
                    session: Session::new(SessionId::new(2), &spec),
                    spec,
                    command,
                    scrollback: b"second".to_vec(),
                    queue: Default::default(),
                },
            ],
            archives: Vec::new(),
            extra: BTreeMap::new(),
        };
        snapshot.write(&path).expect("write");

        let broken = sidecar_path(&path, &SessionId::new(1));
        fs::remove_file(&broken).expect("remove first sidecar");
        fs::create_dir(&broken).expect("replace first sidecar with a directory");

        let restored = SessionStoreSnapshot::read(&path)
            .expect("read store")
            .expect("snapshot");
        assert_eq!(restored.sessions.len(), 2);
        assert!(restored.sessions[0].scrollback.is_empty());
        assert_eq!(restored.sessions[1].scrollback, b"second");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_names_are_injective_for_restored_ids() {
        use std::collections::HashSet;

        let ids = ["", "session", "a.", "a_2e", "_", "_5f", "../../evil"];
        let names: HashSet<_> = ids
            .iter()
            .map(|id| sidecar_name(&SessionId((*id).into())))
            .collect();
        assert_eq!(names.len(), ids.len(), "{names:?}");
        assert_eq!(sidecar_name(&SessionId("a.".into())), "a_2e");
        assert_eq!(sidecar_name(&SessionId("a_2e".into())), "a_5f2e");
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

    fn archive_at(sequence: u64, at: Option<SystemTime>) -> ArchivedSession {
        ArchivedSession {
            id: SessionId::new(sequence),
            agent: Agent::Claude,
            name: format!("row-{sequence}"),
            cwd: PathBuf::from("."),
            command: "claude.exe".into(),
            archived_at: at,
        }
    }

    #[test]
    fn the_count_bound_drops_the_oldest_records_first() {
        let now = SystemTime::now();
        let mut archives: Vec<_> = (0..MAX_ARCHIVES as u64 + 40)
            .map(|sequence| archive_at(sequence, Some(now)))
            .collect();
        trim_archives(&mut archives, now);
        assert_eq!(archives.len(), MAX_ARCHIVES);
        // The 40 that went is the front of the list, not the back: the newest
        // archive is the one an operator is most likely to want back.
        assert_eq!(archives[0].id, SessionId::new(40));
        assert_eq!(
            archives[MAX_ARCHIVES - 1].id,
            SessionId::new(MAX_ARCHIVES as u64 + 39)
        );
    }

    #[test]
    fn the_age_bound_applies_below_the_count_bound() {
        let now = SystemTime::now();
        let stale = now - (ARCHIVE_MAX_AGE + Duration::from_secs(60));
        let mut archives = vec![
            archive_at(1, Some(stale)),
            archive_at(2, Some(now - Duration::from_secs(60))),
        ];
        trim_archives(&mut archives, now);
        assert_eq!(archives.len(), 1, "a month-old archive is not kept");
        assert_eq!(archives[0].id, SessionId::new(2));
    }

    #[test]
    fn an_unstamped_record_is_kept_until_the_count_bound_reaches_it() {
        // A store written before the stamp existed must not read as archived at
        // the epoch, or upgrading would delete the whole history at once.
        let now = SystemTime::now();
        let mut archives = vec![archive_at(1, None)];
        trim_archives(&mut archives, now);
        assert_eq!(archives.len(), 1);

        let mut many: Vec<_> = (0..MAX_ARCHIVES as u64 + 1)
            .map(|sequence| archive_at(sequence, None))
            .collect();
        trim_archives(&mut many, now);
        assert_eq!(many.len(), MAX_ARCHIVES);
    }

    #[test]
    fn a_clock_that_moved_backwards_does_not_delete_history() {
        let now = SystemTime::now();
        let mut archives = vec![archive_at(1, Some(now + Duration::from_secs(3600)))];
        trim_archives(&mut archives, now);
        assert_eq!(archives.len(), 1);
    }
}
