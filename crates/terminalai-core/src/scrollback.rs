//! Session output beyond what memory holds.
//!
//! Every session keeps a bounded ring of recent bytes in memory, because that
//! is what a reattach replays into xterm and what the fleet row's last line is
//! read from. The ring is small on purpose: thirty tracked sessions share one
//! process, and the memory budget is the reason this tool scales past four
//! panes at all. Anything older than the ring used to be gone.
//!
//! This module is the tier underneath it. Three properties shape it:
//!
//! - **Bounded in bytes, never in lines.** A line limit costs whatever the pane
//!   is wide, so the same "10,000 lines" is three times the storage at 360
//!   columns as at 120 — the operator sets a number that does not mean anything
//!   (tmux#4859). Bytes are the thing actually being spent.
//! - **The hot path never touches a file.** Output arrives on the pty reader
//!   thread while the registry's state lock is held; a blocking write there
//!   stalls every other session and applies backpressure to the agent itself.
//!   Appends are handed to a writer thread through a bounded queue.
//! - **A gap is recorded, not hidden.** If the queue fills, bytes are dropped
//!   rather than blocking the fleet — but the log says so, in place. Scrollback
//!   that quietly omits a stretch of output is worse than scrollback that
//!   admits it, because only one of them can be reasoned about.
//!
//! Storage is two segments per session, rotated. Truncating a single file from
//! the front would rewrite the whole thing on every rotation; two files bound
//! the total at twice the segment size and cost one rename.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::session::SessionId;

/// Total bytes of history kept on disk per session, across both segments.
///
/// Sixteen times the in-memory ring: enough to hold a long agent run's output,
/// small enough that thirty sessions cost a quarter of a gigabyte of disk in
/// the worst case and nothing in the common one, since files are only as large
/// as what was written.
pub const MAX_DISK_SCROLLBACK_BYTES: u64 = 8 * 1024 * 1024;

/// One segment is half the budget: the active file plus the retained previous
/// one is the bound.
const SEGMENT_BYTES: u64 = MAX_DISK_SCROLLBACK_BYTES / 2;

/// Chunks the writer may fall behind by before appends start being dropped.
///
/// The pty reader hands over at most 8 KiB at a time, so this is a few
/// megabytes of slack — far more than a disk needs to catch up, and bounded so
/// a stalled disk cannot grow this process without limit.
const QUEUE_CAPACITY: usize = 512;

/// What the writer thread is asked to do.
enum Message {
    Append { id: SessionId, bytes: Vec<u8> },
    Forget { id: SessionId },
    Flush(SyncSender<()>),
}

/// The disk tier for every session's scrollback.
///
/// Cloning is cheap and shares one writer thread. Dropping the last handle
/// stops the thread after it drains what it was given.
pub struct ScrollbackSpool {
    sender: SyncSender<Message>,
    /// Bytes the queue or disk refused, awaiting a gap marker in the matching
    /// session's log.
    dropped: Arc<Mutex<HashMap<SessionId, u64>>>,
    directory: PathBuf,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ScrollbackSpool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScrollbackSpool")
            .field("directory", &self.directory)
            .field("dropped", &self.dropped_bytes())
            .finish()
    }
}

impl ScrollbackSpool {
    /// Start a spool writing under `directory`, creating it if needed.
    pub fn new(directory: impl Into<PathBuf>) -> std::io::Result<Self> {
        let directory = directory.into();
        std::fs::create_dir_all(&directory)?;
        let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
        let dropped = Arc::new(Mutex::new(HashMap::new()));
        let worker = std::thread::Builder::new()
            .name("terminalai-scrollback".into())
            .spawn({
                let directory = directory.clone();
                let dropped = Arc::clone(&dropped);
                move || run_writer(&directory, receiver, &dropped)
            })?;
        Ok(Self {
            sender,
            dropped,
            directory,
            worker: Some(worker),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Queue bytes for one session. Never blocks and never fails.
    ///
    /// This runs under the registry's state lock. A full queue means the disk
    /// is not keeping up with thirty agents at once; the fleet keeps running
    /// and the log records what it lost.
    pub fn append(&self, id: &SessionId, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let message = Message::Append {
            id: id.clone(),
            bytes: bytes.to_vec(),
        };
        match self.sender.try_send(message) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.record_drop(id, bytes.len() as u64);
            }
        }
    }

    /// Delete one session's history. Called when a session is removed, so a
    /// closed session does not keep paying for disk.
    pub fn forget(&self, id: &SessionId) {
        self.dropped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
        let _ = self.sender.try_send(Message::Forget { id: id.clone() });
    }

    /// Block until everything queued so far has been written.
    ///
    /// Only for tests and shutdown: the point of the queue is that callers do
    /// not wait for the disk.
    pub fn flush(&self) {
        let (sender, receiver) = sync_channel(1);
        if self.sender.send(Message::Flush(sender)).is_ok() {
            let _ = receiver.recv();
        }
    }

    /// The tail of one session's history, newest bytes last, at most
    /// `max_bytes`.
    ///
    /// Reads the files directly rather than asking the writer thread, so a
    /// caller loading history never waits behind a queue of appends. A read
    /// racing an append sees a prefix of what was written, never a torn record:
    /// appends are whole chunks and the file only grows.
    pub fn history(&self, id: &SessionId, max_bytes: u64) -> Vec<u8> {
        read_history(&self.directory, id, max_bytes)
    }

    /// Bytes dropped because the writer could not keep up or the disk rejected
    /// them, since the process started. Non-zero means a gap is pending or has
    /// not yet been announced by a later successful append.
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .copied()
            .sum()
    }

    fn record_drop(&self, id: &SessionId, bytes: u64) {
        record_drop(&self.dropped, id, bytes);
    }
}

impl Drop for ScrollbackSpool {
    fn drop(&mut self) {
        // Dropping the sender ends the writer's loop; joining means the last
        // bytes of a shutting-down fleet are on disk rather than in a queue
        // that went away with the process.
        let (sender, receiver) = sync_channel(1);
        if self.sender.send(Message::Flush(sender)).is_ok() {
            let _ = receiver.recv();
        }
        if let Some(worker) = self.worker.take() {
            drop(std::mem::replace(&mut self.sender, sync_channel(1).0));
            let _ = worker.join();
        }
    }
}

const DISK_WARNING_INTERVAL: Duration = Duration::from_secs(60);

fn run_writer(
    directory: &Path,
    receiver: Receiver<Message>,
    dropped: &Mutex<HashMap<SessionId, u64>>,
) {
    let mut logs: HashMap<SessionId, ScrollbackLog> = HashMap::new();
    let mut last_warning = None;
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Append { id, bytes } => {
                let log = logs
                    .entry(id.clone())
                    .or_insert_with(|| ScrollbackLog::new(directory, &id));
                // Read-and-clear: whatever was dropped before this chunk is
                // announced immediately before it, so the marker sits at the
                // point in the stream where the bytes are actually missing.
                let missing = take_dropped(dropped, &id);
                if missing > 0 {
                    if let Err(error) = log.append(gap_marker(missing).as_bytes()) {
                        record_drop(dropped, &id, missing);
                        record_drop(dropped, &id, bytes.len() as u64);
                        warn_append_error(&mut last_warning, &id, &error);
                        continue;
                    }
                }
                if let Err(error) = log.append(&bytes) {
                    record_drop(dropped, &id, bytes.len() as u64);
                    warn_append_error(&mut last_warning, &id, &error);
                }
            }
            Message::Forget { id } => {
                dropped
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&id);
                logs.remove(&id);
                remove_segments(directory, &id);
            }
            Message::Flush(ack) => {
                for log in logs.values_mut() {
                    log.flush();
                }
                let _ = ack.send(());
            }
        }
    }
}

fn record_drop(dropped: &Mutex<HashMap<SessionId, u64>>, id: &SessionId, bytes: u64) {
    let mut dropped = dropped
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = dropped.entry(id.clone()).or_default();
    *entry = entry.saturating_add(bytes);
}

fn take_dropped(dropped: &Mutex<HashMap<SessionId, u64>>, id: &SessionId) -> u64 {
    dropped
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(id)
        .unwrap_or_default()
}

fn warn_append_error(
    last_warning: &mut Option<Instant>,
    id: &SessionId,
    error: &std::io::Error,
) {
    let now = Instant::now();
    if last_warning.is_none_or(|last| {
        now.duration_since(last) >= DISK_WARNING_INTERVAL
    }) {
        tracing::warn!(session = %id, error = %error, "could not append scrollback; output is marked as dropped");
        *last_warning = Some(now);
    }
}

/// What the operator sees where output is missing. Deliberately unmistakable:
/// a silent gap would be read as the agent having said nothing.
fn gap_marker(bytes: u64) -> String {
    format!("\r\n[terminalai: {bytes} bytes of scrollback dropped — the disk fell behind]\r\n")
}

/// One session's history: an active segment and the one before it.
struct ScrollbackLog {
    active_path: PathBuf,
    previous_path: PathBuf,
    file: Option<File>,
    written: u64,
}

impl ScrollbackLog {
    fn new(directory: &Path, id: &SessionId) -> Self {
        let (active_path, previous_path) = segment_paths(directory, id);
        let written = std::fs::metadata(&active_path).map(|meta| meta.len()).unwrap_or(0);
        Self {
            active_path,
            previous_path,
            file: None,
            written,
        }
    }

    fn open(&mut self) -> std::io::Result<&mut File> {
        if self.file.is_none() {
            if let Some(parent) = self.active_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.active_path)?;
            self.written = file.metadata().map(|meta| meta.len()).unwrap_or(self.written);
            self.file = Some(file);
        }
        Ok(self.file.as_mut().expect("just opened"))
    }

    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if self.written.saturating_add(bytes.len() as u64) > SEGMENT_BYTES {
            self.rotate()?;
        }
        let file = self.open()?;
        file.write_all(bytes)?;
        self.written = self.written.saturating_add(bytes.len() as u64);
        Ok(())
    }

    /// Retire the active segment and start an empty one.
    ///
    /// A chunk larger than a whole segment would otherwise rotate on every
    /// append and still not fit; it is written to a fresh segment regardless,
    /// so the bound is "twice the segment plus one chunk" rather than a loop.
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        if self.active_path.exists() {
            std::fs::rename(&self.active_path, &self.previous_path)?;
        }
        self.written = 0;
        Ok(())
    }

    fn flush(&mut self) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
    }
}

fn segment_paths(directory: &Path, id: &SessionId) -> (PathBuf, PathBuf) {
    let name = segment_name(id);
    (
        directory.join(format!("{name}.log")),
        directory.join(format!("{name}.1.log")),
    )
}

/// A session id is operator-visible text, so it is escaped rather than trusted
/// as a file name — the same rule the session store's sidecars use.
fn segment_name(id: &SessionId) -> String {
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

fn remove_segments(directory: &Path, id: &SessionId) {
    let (active, previous) = segment_paths(directory, id);
    let _ = std::fs::remove_file(active);
    let _ = std::fs::remove_file(previous);
}

/// The last `max_bytes` of a session's history, oldest segment first.
pub fn read_history(directory: &Path, id: &SessionId, max_bytes: u64) -> Vec<u8> {
    if max_bytes == 0 {
        return Vec::new();
    }
    let (active, previous) = segment_paths(directory, id);
    let mut out = Vec::new();
    // The active segment is what the caller most wants; read it first so a
    // budget smaller than one segment spends everything on the newest bytes.
    let newest = read_tail(&active, max_bytes);
    let remaining = max_bytes.saturating_sub(newest.len() as u64);
    if remaining > 0 {
        out = read_tail(&previous, remaining);
    }
    out.extend_from_slice(&newest);
    out
}

fn read_tail(path: &Path, max_bytes: u64) -> Vec<u8> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(length) = file.metadata().map(|meta| meta.len()) else {
        return Vec::new();
    };
    let start = length.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((length - start) as usize);
    if file.take(max_bytes).read_to_end(&mut out).is_err() {
        return Vec::new();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-spool-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Scratch(dir)
    }

    #[test]
    fn history_outlives_the_memory_ring() {
        // The whole point: bytes the in-memory ring has already discarded are
        // still readable.
        let dir = scratch("outlives");
        let spool = ScrollbackSpool::new(&dir.0).expect("spool");
        let id = SessionId::new(1);
        spool.append(&id, b"the first thing it said\n");
        spool.append(&id, b"the last thing it said\n");
        spool.flush();
        let history = spool.history(&id, 1024);
        let text = String::from_utf8_lossy(&history);
        assert!(text.contains("the first thing"), "{text}");
        assert!(text.contains("the last thing"), "{text}");
    }

    #[test]
    fn a_budget_smaller_than_the_history_keeps_the_newest_bytes() {
        let dir = scratch("tail");
        let spool = ScrollbackSpool::new(&dir.0).expect("spool");
        let id = SessionId::new(1);
        spool.append(&id, b"oldest");
        spool.append(&id, b"newest");
        spool.flush();
        assert_eq!(spool.history(&id, 6), b"newest".to_vec());
    }

    #[test]
    fn disk_use_stays_bounded_across_a_rotation() {
        // A session that runs for hours must not fill the disk. Written well
        // past the budget, the two segments together stay under it.
        let dir = scratch("bounded");
        let id = SessionId::new(1);
        let mut log = ScrollbackLog::new(&dir.0, &id);
        let chunk = vec![b'x'; 64 * 1024];
        let mut written = 0u64;
        while written < MAX_DISK_SCROLLBACK_BYTES * 2 {
            log.append(&chunk).expect("append");
            written += chunk.len() as u64;
        }
        log.flush();
        let (active, previous) = segment_paths(&dir.0, &id);
        let on_disk = std::fs::metadata(&active).map(|m| m.len()).unwrap_or(0)
            + std::fs::metadata(&previous).map(|m| m.len()).unwrap_or(0);
        assert!(
            on_disk <= MAX_DISK_SCROLLBACK_BYTES + chunk.len() as u64,
            "{on_disk} bytes on disk after writing {written}"
        );
        assert!(on_disk > SEGMENT_BYTES, "rotation discarded everything");
    }

    #[test]
    fn a_rotation_keeps_the_newest_bytes_and_discards_the_oldest() {
        let dir = scratch("rotate");
        let id = SessionId::new(1);
        let mut log = ScrollbackLog::new(&dir.0, &id);
        log.append(b"FIRST").expect("append");
        log.append(&vec![b'x'; SEGMENT_BYTES as usize]).expect("append");
        log.append(b"LATEST").expect("append");
        log.flush();
        let history = read_history(&dir.0, &id, MAX_DISK_SCROLLBACK_BYTES);
        assert!(history.ends_with(b"LATEST"), "newest bytes lost");
        // Two rotations later the first write is gone, which is the bound
        // doing its job rather than a failure.
        log.append(&vec![b'y'; SEGMENT_BYTES as usize]).expect("append");
        log.append(&vec![b'z'; SEGMENT_BYTES as usize]).expect("append");
        log.flush();
        let history = read_history(&dir.0, &id, MAX_DISK_SCROLLBACK_BYTES);
        assert!(!history.starts_with(b"FIRST"));
    }

    #[test]
    fn dropped_bytes_are_announced_in_place_rather_than_omitted() {
        // Scrollback with an unmarked hole in it cannot be reasoned about: the
        // operator reads it as the agent having gone quiet.
        let dir = scratch("gap");
        let spool = ScrollbackSpool::new(&dir.0).expect("spool");
        let id = SessionId::new(1);
        spool.append(&id, b"before\n");
        spool.flush();
        spool.record_drop(&id, 4096);
        spool.append(&id, b"after\n");
        spool.flush();
        let text = String::from_utf8_lossy(&spool.history(&id, 4096)).into_owned();
        let gap = text.find("4096 bytes of scrollback dropped").expect("marker");
        let before = text.find("before").expect("before");
        let after = text.find("after").expect("after");
        assert!(before < gap && gap < after, "marker misplaced: {text}");
        assert_eq!(spool.dropped_bytes(), 0, "counter not cleared");
    }

    #[test]
    fn dropped_bytes_are_marked_in_the_session_that_lost_them() {
        let dir = scratch("per-session-gap");
        let spool = ScrollbackSpool::new(&dir.0).expect("spool");
        let owner = SessionId::new(1);
        let other = SessionId::new(2);
        spool.record_drop(&owner, 4096);
        spool.append(&other, b"other\n");
        spool.append(&owner, b"owner\n");
        spool.flush();

        let owner_text = String::from_utf8_lossy(&spool.history(&owner, 4096)).into_owned();
        let other_text = String::from_utf8_lossy(&spool.history(&other, 4096)).into_owned();
        assert!(owner_text.contains("4096 bytes of scrollback dropped"));
        assert!(!other_text.contains("scrollback dropped"), "{other_text}");
    }

    #[test]
    fn an_append_error_is_marked_when_the_disk_recovers() {
        let dir = scratch("append-error");
        let spool = ScrollbackSpool::new(&dir.0).expect("spool");
        let id = SessionId::new(1);
        let (active, _) = segment_paths(&dir.0, &id);
        std::fs::create_dir(&active).expect("block the active log path");
        spool.append(&id, b"lost");
        spool.flush();
        assert_eq!(spool.dropped_bytes(), 4, "failed append was not retained");

        std::fs::remove_dir(&active).expect("unblock the active log path");
        spool.append(&id, b"recovered");
        spool.flush();
        let text = String::from_utf8_lossy(&spool.history(&id, 4096)).into_owned();
        let gap = text.find("4 bytes of scrollback dropped").expect("marker");
        let recovered = text.find("recovered").expect("recovered output");
        assert!(gap < recovered, "marker misplaced: {text}");
        assert_eq!(spool.dropped_bytes(), 0, "counter not cleared");
    }

    #[test]
    fn forgetting_a_session_removes_its_files() {
        let dir = scratch("forget");
        let spool = ScrollbackSpool::new(&dir.0).expect("spool");
        let id = SessionId::new(1);
        spool.append(&id, b"something");
        spool.flush();
        let (active, _) = segment_paths(&dir.0, &id);
        assert!(active.exists());
        spool.forget(&id);
        spool.flush();
        assert!(!active.exists(), "a closed session kept paying for disk");
        assert!(spool.history(&id, 1024).is_empty());
    }

    #[test]
    fn a_session_id_never_escapes_the_spool_directory() {
        // Ids reach here from a restored store; a traversal would write outside
        // the directory the daemon owns.
        let name = segment_name(&SessionId("../../evil".into()));
        assert!(!name.contains('.') && !name.contains('/') && !name.contains('\\'), "{name}");
    }

    #[test]
    fn session_id_filename_escaping_is_injective() {
        use std::collections::HashSet;

        let ids = ["", "session", "a.", "a_2e", "_", "_5f", "../../evil"];
        let names: HashSet<_> = ids
            .iter()
            .map(|id| segment_name(&SessionId((*id).into())))
            .collect();
        assert_eq!(names.len(), ids.len(), "{names:?}");
        assert_eq!(segment_name(&SessionId("a.".into())), "a_2e");
        assert_eq!(segment_name(&SessionId("a_2e".into())), "a_5f2e");
    }

    #[test]
    fn history_for_a_session_that_never_wrote_is_empty_not_an_error() {
        let dir = scratch("missing");
        let spool = ScrollbackSpool::new(&dir.0).expect("spool");
        assert!(spool.history(&SessionId::new(9), 1024).is_empty());
    }
}
