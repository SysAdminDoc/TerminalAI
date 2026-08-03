use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use terminalai_core::SessionStoreSnapshot;

const STORE_DEBOUNCE: Duration = Duration::from_millis(200);

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
    sender: SyncSender<SessionStoreSnapshot>,
}

impl StoreWriter {
    pub(crate) fn spawn(path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = thread::Builder::new()
            .name("terminalai-session-store".into())
            .spawn(move || run_writer(&path, receiver));
        Self { sender }
    }

    pub(crate) fn update(&self, snapshot: SessionStoreSnapshot) {
        match self.sender.try_send(snapshot) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

pub(crate) fn default_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("TerminalAI").join("sessions.json"))
}

pub(crate) fn load(path: &Path) -> Result<SessionStoreSnapshot, String> {
    SessionStoreSnapshot::read(path)
        .map(|snapshot| snapshot.unwrap_or_default())
        .map_err(|error| error.to_string())
}

fn run_writer(path: &Path, receiver: mpsc::Receiver<SessionStoreSnapshot>) {
    while let Ok(mut snapshot) = receiver.recv() {
        while let Ok(next) = receiver.recv_timeout(STORE_DEBOUNCE) {
            snapshot = next;
        }
        if let Err(error) = snapshot.write(path) {
            eprintln!("terminalai-daemon: could not persist session store: {error}");
        }
    }
}
