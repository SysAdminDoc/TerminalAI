use std::fs::{self, create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use terminalai_core::{SessionRegistry, SessionStoreSnapshot};

const STORE_DEBOUNCE: Duration = Duration::from_millis(200);
const STORE_MAX_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct LoadResult {
    pub(crate) snapshot: SessionStoreSnapshot,
    pub(crate) quarantined_path: Option<PathBuf>,
}

pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_crash_record(info);
        previous(info);
    }));
}

fn crash_log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("TerminalAI").join("crash.log"))
}

fn write_crash_record(info: &std::panic::PanicHookInfo<'_>) {
    let Some(path) = crash_log_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if create_dir_all(parent).is_err() {
        return;
    }
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let record = serde_json::json!({
        "timestamp_ms": timestamp_ms,
        "process_id": std::process::id(),
        "thread": std::thread::current().name().unwrap_or("unnamed"),
        "panic": info.to_string(),
        "backtrace": std::backtrace::Backtrace::force_capture().to_string(),
    });
    let Ok(encoded) = serde_json::to_string(&record) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{encoded}");
}

#[derive(Clone)]
pub(crate) struct StoreWriter {
    sender: SyncSender<()>,
}

impl StoreWriter {
    pub(crate) fn spawn(path: PathBuf, registry: SessionRegistry) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = thread::Builder::new()
            .name("terminalai-session-store".into())
            .spawn(move || run_writer(&path, registry, receiver));
        Self { sender }
    }

    pub(crate) fn update(&self) {
        match self.sender.try_send(()) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

pub(crate) fn default_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("TerminalAI").join("sessions.json"))
}

pub(crate) fn load(path: &Path) -> Result<LoadResult, String> {
    match SessionStoreSnapshot::read(path) {
        Ok(snapshot) => Ok(LoadResult {
            snapshot: snapshot.unwrap_or_default(),
            quarantined_path: None,
        }),
        Err(error) => {
            let quarantined_path = match quarantine(path) {
                Ok(quarantined_path) => {
                    eprintln!(
                        "terminalai-daemon: quarantined unreadable session store ({error}) at {}",
                        quarantined_path.display()
                    );
                    Some(quarantined_path)
                }
                Err(quarantine_error) => {
                    eprintln!(
                        "terminalai-daemon: session store is unreadable ({error}); could not quarantine {}: {quarantine_error}",
                        path.display(),
                    );
                    None
                }
            };
            Ok(LoadResult {
                snapshot: SessionStoreSnapshot::default(),
                quarantined_path,
            })
        }
    }
}

fn quarantine(path: &Path) -> std::io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stamp = file_safe_utc_timestamp(SystemTime::now());
    let base_name = format!("sessions.corrupt-{stamp}.json");
    let mut candidate = parent.join(&base_name);
    for suffix in 1..1000 {
        match fs::rename(path, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                candidate = parent.join(format!("sessions.corrupt-{stamp}-{suffix}.json"));
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not choose a unique quarantine path",
    ))
}

/// RFC 3339 uses colons in its time component, which Windows rejects in file names.
/// Keep the same UTC date/time fields while replacing only those separators with hyphens.
fn file_safe_utc_timestamp(now: SystemTime) -> String {
    let seconds = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}Z")
}

fn run_writer(path: &Path, registry: SessionRegistry, receiver: mpsc::Receiver<()>) {
    while receiver.recv().is_ok() {
        let max_deadline = Instant::now() + STORE_MAX_INTERVAL;
        let mut quiet_deadline = Instant::now() + STORE_DEBOUNCE;
        let disconnected = loop {
            let now = Instant::now();
            let until_max = max_deadline.saturating_duration_since(now);
            let until_quiet = quiet_deadline.saturating_duration_since(now);
            if until_max.is_zero() {
                break false;
            }
            match receiver.recv_timeout(until_max.min(until_quiet)) {
                Ok(()) => quiet_deadline = Instant::now() + STORE_DEBOUNCE,
                Err(mpsc::RecvTimeoutError::Timeout) => break false,
                Err(mpsc::RecvTimeoutError::Disconnected) => break true,
            }
        };
        let snapshot = registry.store_snapshot();
        if let Err(error) = snapshot.write(path) {
            eprintln!("terminalai-daemon: could not persist session store: {error}");
        }
        if disconnected {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use terminalai_core::store::{ArchivedSession, StoredSession};
    use terminalai_core::{Agent, LaunchSpec, ResolvedCommand, Session, SessionId};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-daemon-store-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    fn assert_fixture_is_quarantined(name: &str) {
        let dir = test_dir();
        let path = dir.join("sessions.json");
        let fixture = match name {
            "truncated" => include_str!("../tests/fixtures/store/truncated.json"),
            "schema-0" => include_str!("../tests/fixtures/store/schema-0.json"),
            "schema-999" => include_str!("../tests/fixtures/store/schema-999.json"),
            _ => panic!("unknown fixture"),
        };
        fs::write(&path, fixture).expect("seed store");

        let loaded = load(&path).expect("load should recover");
        assert!(loaded.snapshot.sessions.is_empty());
        assert!(loaded.snapshot.archives.is_empty());
        let quarantined = loaded.quarantined_path.expect("quarantine path");
        let file_name = quarantined
            .file_name()
            .and_then(|name| name.to_str())
            .expect("quarantine file name");
        assert!(file_name.starts_with("sessions.corrupt-"));
        assert!(file_name.ends_with(".json"));
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(quarantined).expect("quarantine contents"),
            fixture
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn truncated_store_is_quarantined_and_loads_empty() {
        assert_fixture_is_quarantined("truncated");
    }

    #[test]
    fn old_store_schema_is_quarantined_and_loads_empty() {
        assert_fixture_is_quarantined("schema-0");
    }

    #[test]
    fn future_store_schema_is_quarantined_and_loads_empty() {
        assert_fixture_is_quarantined("schema-999");
    }

    #[test]
    fn update_wakes_a_worker_that_snapshots_the_registry_after_debounce() {
        let dir = test_dir();
        let path = dir.join("sessions.json");
        let cwd = std::env::current_dir().expect("cwd");
        let spec = LaunchSpec {
            agent: Agent::Claude,
            cwd: cwd.clone(),
            ..LaunchSpec::default()
        };
        let session = Session::new(SessionId::new(1), &spec);
        let registry = SessionRegistry::from_store(SessionStoreSnapshot {
            schema_version: terminalai_core::store::SESSION_STORE_SCHEMA_VERSION,
            sessions: vec![StoredSession {
                session,
                spec: spec.clone(),
                command: ResolvedCommand {
                    program: "claude.exe".into(),
                    args: Vec::new(),
                    cwd,
                },
                scrollback: b"worker-built".to_vec(),
            }],
            archives: vec![ArchivedSession {
                id: SessionId::new(8),
                agent: Agent::Codex,
                name: "archived".into(),
                cwd: spec.cwd.clone(),
                command: "codex.exe".into(),
            }],
        });
        let writer = StoreWriter::spawn(path.clone(), registry);
        writer.update();
        thread::sleep(STORE_DEBOUNCE + Duration::from_millis(100));
        drop(writer);

        let written = SessionStoreSnapshot::read(&path)
            .expect("read worker snapshot")
            .expect("snapshot exists");
        assert_eq!(written.sessions[0].scrollback, b"worker-built");
        assert_eq!(written.archives.len(), 1);
        assert!(!fs::read_to_string(&path)
            .expect("read metadata")
            .contains("scrollback"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sustained_updates_force_a_current_snapshot_and_keep_the_final_state() {
        let dir = test_dir();
        let path = dir.join("sessions.json");
        let cwd = std::env::current_dir().expect("cwd");
        let spec = LaunchSpec {
            agent: Agent::Claude,
            cwd: cwd.clone(),
            ..LaunchSpec::default()
        };
        let id = SessionId::new(1);
        let registry = SessionRegistry::from_store(SessionStoreSnapshot {
            schema_version: terminalai_core::store::SESSION_STORE_SCHEMA_VERSION,
            sessions: vec![StoredSession {
                session: Session::new(id.clone(), &spec),
                spec: spec.clone(),
                command: ResolvedCommand {
                    program: "claude.exe".into(),
                    args: Vec::new(),
                    cwd,
                },
                scrollback: Vec::new(),
            }],
            archives: Vec::new(),
        });
        let writer = StoreWriter::spawn(path.clone(), registry.clone());
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            writer.update();
            thread::sleep(Duration::from_millis(25));
        }
        registry.mark_reviewed(&id).expect("mark final state");
        writer.update();
        thread::sleep(STORE_MAX_INTERVAL + STORE_DEBOUNCE + Duration::from_millis(100));
        drop(writer);

        let written = SessionStoreSnapshot::read(&path)
            .expect("read sustained snapshot")
            .expect("snapshot exists");
        assert!(written.sessions[0].session.reviewed);
        let _ = fs::remove_dir_all(dir);
    }
}
