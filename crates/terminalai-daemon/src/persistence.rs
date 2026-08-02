use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::Duration;

use terminalai_core::SessionStoreSnapshot;

const STORE_DEBOUNCE: Duration = Duration::from_millis(200);

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
