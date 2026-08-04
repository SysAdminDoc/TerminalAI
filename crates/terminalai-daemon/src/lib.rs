//! The TerminalAI control plane.
//!
//! The daemon is the only process that owns [`terminalai_core::PtySession`]
//! values. The GUI and CLI are clients of a versioned, line-framed protocol
//! over a local socket (a Windows named pipe on the primary platform). Events
//! are sent only after an explicit `Subscribe` request, so broadcasts cannot be
//! mistaken for RPC responses.
//!
//! # Trust boundary
//!
//! On Windows, the named-pipe DACL grants access to the current interactive
//! user's SID and `SYSTEM` only. That DACL is the authorization boundary. The
//! PID in `Request::Hello` is a diagnostic consistency check supplied by the
//! client; it is not an authorization mechanism, and a peer cannot gain access
//! by declaring a different PID. Session setup and teardown hooks are separate,
//! explicit per-session inputs and execute local shell code in the project
//! directory when the operator supplies them.
//!
//! # The daemon must unwind
//!
//! This process runs a thread per connection, per writer, per PTY reader, per
//! restart timer and per process monitor, and it is the sole owner of every live
//! agent. Under `panic = "abort"` a single panic on any one of those threads
//! terminates every supervised session at once, and the recovery arms for
//! poisoned locks become dead code in the shipped binary while the test profile
//! silently forces unwinding — so those tests would pass without proving
//! anything. The guard below fails the build rather than shipping that.

#[cfg(panic = "abort")]
compile_error!(
    "terminalai-daemon must be built with unwinding: `panic = \"abort\"` makes one panic on any \
     worker thread kill every supervised session, and turns the poisoned-lock recovery paths into \
     dead code. Remove `panic = \"abort\"` from the active cargo profile."
);

mod http_hooks;
mod logging;
mod persistence;

pub use http_hooks::HookEndpoint;
pub use logging::{
    init_logging, init_logging_with_prefix, log_directory, LogHub, LoggingGuard, MAX_LOG_FILES,
};

#[cfg(feature = "codex-app-server")]
pub mod app_server;

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(windows)]
use std::sync::Condvar;
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::{
    prelude::*, GenericNamespaced, ListenerNonblockingMode, ListenerOptions, Name, RecvHalf,
    SendHalf,
};
use serde::{Deserialize, Serialize};
use terminalai_core::agent::{self, Agent, Origin};
use terminalai_core::land::LandQueue;
use terminalai_core::launch::LaunchSpec;
use terminalai_core::pty::PtySize;
use terminalai_core::scrollback::ScrollbackSpool;
use terminalai_core::{
    AdmissionConfig, AdmissionSnapshot, AgentEvent, HookEvent, LogEntry, RegistryEvent, ReviewItem,
    Session, SessionId, SessionRegistry,
};

use persistence::StoreWriter;

pub const PROTOCOL_VERSION: u16 = 3;
/// Stable control-plane name. Protocol compatibility is negotiated in the
/// first frame, so changing the socket name would strand an older daemon that
/// still owns live sessions before the newer client can report the skew.
pub const PIPE_NAME: &str = "terminalai.control";
const LEGACY_PIPE_NAME: &str = "terminalai.control.v2";
const OUTGOING_QUEUE_CAPACITY: usize = 256;
/// Largest control frame either end will read.
///
/// A peer that sends bytes without a newline would otherwise grow a `String`
/// until the process is out of memory — and this is a local control plane, so
/// the peer is a program, not a person. The cap is generously above the largest
/// legitimate frame: a `ScrollbackHistory` response carries a bounded history
/// as a JSON array of numbers, which is several times larger on the wire than
/// its raw bytes.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Largest `Write` payload accepted for one session. Rejected, never truncated:
/// half a prompt reaching an agent is worse than none.
pub const MAX_WRITE_BYTES: usize = 256 * 1024;
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Most history one request may return: the in-memory ring plus one older
/// window, so the UI can replay bytes from before the pane's existing tail.
///
/// A response is one frame, and a frame is capped at [`MAX_FRAME_BYTES`]. The
/// bytes are JSON-encoded as an array of numbers, so the frame ceiling remains
/// several times larger than the raw history budget.
pub const MAX_HISTORY_BYTES: u64 =
    terminalai_core::registry::MAX_SCROLLBACK_BYTES as u64 + 128 * 1024;
/// Most sessions one broadcast may target.
///
/// Well above the fleet's ~30-session design point, and bounded so a malformed
/// client cannot make the daemon iterate an arbitrary list while holding the
/// registry lock once per entry.
pub const MAX_BROADCAST_TARGETS: usize = 256;

/// Sessions the supervisor did not start, reconciled from the agent's own
/// registry with the CLI as a fallback when that registry is unreadable.
///
/// Never fabricates: an empty result means nothing was discoverable, which the
/// UI renders as "unknown", not as an empty machine.
fn external_sessions() -> Vec<terminalai_core::ExternalSession> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    external_sessions_from(&home, || {
        // The registry directory was missing or unreadable. Ask the CLI once.
        agent::resolve(Agent::Claude, None)
            .ok()
            .and_then(|binary| terminalai_core::external::enumerate_via_cli(&binary.path))
            .unwrap_or_default()
    })
}

fn external_sessions_from<F>(home: &Path, fallback: F) -> Vec<terminalai_core::ExternalSession>
where
    F: FnOnce() -> Vec<terminalai_core::ExternalSession>,
{
    match terminalai_core::claude_sessions(home, &terminalai_core::external::process_is_running) {
        Some(sessions) => sessions,
        None => fallback(),
    }
}

/// Read one newline-delimited frame, refusing anything over [`MAX_FRAME_BYTES`].
///
/// `BufRead::read_line` has no upper bound, so a peer that never sends a newline
/// can exhaust memory on either side of this protocol.
fn read_frame<R: BufRead>(reader: &mut R, line: &mut String) -> Result<usize, IpcError> {
    line.clear();
    // One byte over the limit is enough to prove the frame is too long without
    // buffering the rest of it.
    let mut limited = std::io::Read::take(&mut *reader, MAX_FRAME_BYTES as u64 + 1);
    let read = limited.read_line(line)?;
    if read > MAX_FRAME_BYTES {
        return Err(IpcError::InvalidMessage(format!(
            "control frame exceeded {MAX_FRAME_BYTES} bytes"
        )));
    }
    Ok(read)
}

pub fn install_panic_hook() {
    persistence::install_panic_hook();
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("local control transport failed: {0}")]
    Io(#[from] io::Error),
    #[error("control protocol encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("control request timed out")]
    Timeout,
    #[error("control transport lock is poisoned: {0}")]
    Poisoned(&'static str),
    #[error("daemon rejected the request: {0}")]
    Remote(String),
    #[error(
        "incompatible control protocol: running daemon PID {daemon_pid} speaks v{daemon}, client speaks v{client}; stop it with `Stop-Process -Id {daemon_pid}` on Windows, then retry"
    )]
    VersionMismatch {
        daemon: u16,
        client: u16,
        daemon_pid: u32,
    },
    /// Diagnostic only: the pipe DACL authorizes the peer; the declared PID is
    /// not an authorization mechanism and may be self-reported inaccurately.
    #[error(
        "control peer PID diagnostic mismatch: client declared {expected}, transport reported {actual:?}; the pipe DACL remains the authorization boundary"
    )]
    PeerMismatch { expected: u32, actual: Option<u32> },
    #[error("invalid control message: {0}")]
    InvalidMessage(String),
    #[error("session store could not be loaded: {0}")]
    Store(String),
    #[error("invalid admission configuration: {0}")]
    Configuration(String),
}

impl IpcError {
    fn is_version_mismatch(&self) -> bool {
        matches!(self, Self::VersionMismatch { .. })
            || matches!(self, Self::Remote(message) if message
                .starts_with("incompatible control protocol:"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u16,
        client_pid: u32,
    },
    Subscribe,
    Ping,
    Close,
    /// Ask the daemon to tear down owned sessions and leave its accept loop.
    Shutdown,
    Snapshot,
    ReviewSnapshot,
    /// Sessions running outside this supervisor, read from the agent's own
    /// per-PID registry. Read-only: the daemon owns none of them.
    ExternalSessions,
    MarkReviewed {
        id: SessionId,
    },
    /// Sessions this supervisor finished and archived. Read-only history: the
    /// records carry no handle, only enough to relaunch the same command.
    SessionHistory,
    /// Read the daemon-wide admission policy.
    AdmissionConfig,
    /// Replace it without restarting. Running sessions are untouched.
    SetAdmission {
        max_live_sessions: usize,
        default_budget_usd: Option<f64>,
        spend_ceiling_usd: Option<f64>,
        spend_window_hours: Option<f64>,
        memory_budget_mb: Option<u64>,
        session_memory_cap_mb: Option<u64>,
        max_processes_per_session: Option<u32>,
    },
    /// Land a session's uncommitted work into a target repository, or refuse.
    /// Serialised daemon-side so two landings cannot interleave their
    /// precondition checks — the failure a hand-built merge queue works around.
    Land {
        request: Box<terminalai_core::land::LandRequest>,
    },
    Status {
        id: SessionId,
    },
    Resolve {
        agent: Agent,
        configured_path: Option<PathBuf>,
    },
    Capabilities {
        agent: Agent,
        configured_path: Option<PathBuf>,
    },
    Preview {
        spec: Box<LaunchSpec>,
        configured_path: Option<PathBuf>,
    },
    HookEndpoint,
    Hook {
        event: HookEvent,
        /// Secret minted for the supervised session and supplied by its hook
        /// adapter. The daemon-wide HTTP bearer is a separate transport gate.
        #[serde(default)]
        hook_token: Option<String>,
    },
    AgentEvent {
        event: AgentEvent,
    },
    Launch {
        spec: Box<LaunchSpec>,
        configured_path: Option<PathBuf>,
    },
    /// Raw bytes from the user-facing terminal. The registry derives the
    /// focused-pane edit guard from this stream; queue and broadcast writes do
    /// not come through this request.
    Write {
        id: SessionId,
        data: String,
    },
    Resize {
        id: SessionId,
        rows: u16,
        cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    Kill {
        id: SessionId,
    },
    Focus {
        id: Option<SessionId>,
    },
    MarkRead {
        id: SessionId,
    },
    TogglePin {
        id: SessionId,
    },
    /// Prompts waiting their turn on one session.
    QueuedPrompts {
        id: SessionId,
    },
    /// Add a prompt to a session's queue.
    EnqueuePrompt {
        id: SessionId,
        text: String,
    },
    EditQueuedPrompt {
        id: SessionId,
        prompt: u64,
        text: String,
    },
    RemoveQueuedPrompt {
        id: SessionId,
        prompt: u64,
    },
    ReorderQueuedPrompt {
        id: SessionId,
        prompt: u64,
        to: usize,
    },
    PauseQueue {
        id: SessionId,
    },
    ResumeQueue {
        id: SessionId,
    },
    /// Send the same bytes to several sessions at once.
    ///
    /// Answered with one result per session rather than a single status: a
    /// broadcast that says only "ok" or "failed" leaves the operator unable to
    /// tell which agents received the prompt, and re-sending to find out
    /// delivers it twice to the ones that already had it.
    Broadcast {
        ids: Vec<SessionId>,
        data: String,
    },
    Scrollback {
        id: SessionId,
    },
    /// History from the disk tier, reaching past the in-memory ring.
    ///
    /// `max_bytes` is clamped rather than rejected: a client asking for more
    /// than one frame can carry gets what fits, not an error it cannot act on.
    ScrollbackHistory {
        id: SessionId,
        max_bytes: u64,
    },
    /// The parsed terminal state for a pinned or background session.
    ///
    /// Distinct from `Scrollback`, which returns raw bytes for the one focused
    /// renderer. A pinned pane needs a rendered grid it can show without an
    /// xterm instance of its own — the whole reason ~29 rows fit on a screen.
    GridSnapshot {
        id: SessionId,
    },
    Reattach {
        id: SessionId,
    },
    Revive {
        id: SessionId,
    },
    Archive {
        id: SessionId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// Snapshot/status responses intentionally carry the full fleet row model;
// boxing them would add needless wire/API indirection without changing the
// local control-plane contract.
#[allow(clippy::large_enum_variant)]
pub enum Response {
    Hello {
        protocol: u16,
        daemon_pid: u32,
    },
    Admission {
        admission: AdmissionSettings,
    },
    Ok,
    Pong,
    Snapshot {
        sessions: Vec<Session>,
        focused: Option<SessionId>,
        admission: AdmissionSnapshot,
        #[serde(default)]
        store_quarantine: Option<String>,
    },
    Land {
        outcome: terminalai_core::land::LandOutcome,
    },
    ReviewSnapshot {
        entries: Vec<ReviewItem>,
    },
    ExternalSessions {
        sessions: Vec<terminalai_core::ExternalSession>,
    },
    SessionHistory {
        archives: Vec<terminalai_core::ArchivedSession>,
    },
    Status {
        session: Session,
        admission: AdmissionSnapshot,
    },
    Resolved {
        agent: Agent,
        path: PathBuf,
        origin: String,
    },
    Capabilities {
        capabilities: terminalai_core::AgentCapabilities,
    },
    Preview {
        command: String,
    },
    HookEndpoint {
        endpoint: HookEndpoint,
    },
    Hook {
        matched: bool,
    },
    AgentEvent {
        matched: bool,
    },
    Launched {
        id: SessionId,
        queued: bool,
    },
    Scrollback {
        data: Vec<u8>,
    },
    ScrollbackHistory {
        data: Vec<u8>,
    },
    Broadcast {
        results: Vec<terminalai_core::BroadcastResult>,
    },
    QueuedPrompts {
        prompts: Vec<terminalai_core::queue::QueuedPrompt>,
    },
    Enqueued {
        prompt: u64,
    },
    GridSnapshot {
        grid: terminalai_core::TerminalGridSnapshot,
    },
    Reattached {
        data: Vec<u8>,
    },
    Revived {
        id: SessionId,
    },
    Archived {
        id: SessionId,
    },
    PinChanged {
        pinned: bool,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireMessage {
    Request { id: u64, request: Request },
    // Boxed: a `Response` carrying an `AdmissionSnapshot` dwarfs a `Request`,
    // and every slot in the bounded outgoing queue is sized for the largest
    // variant. `Box` is transparent to serde, so the wire format is unchanged.
    Response { id: u64, response: Box<Response> },
    Event { event: RegistryEvent },
}

struct StoreBridge {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl StoreBridge {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for StoreBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct DaemonServer {
    listener: LocalSocketListener,
    registry: SessionRegistry,
    store_bridge: Option<StoreBridge>,
    store_writer: Option<StoreWriter>,
    store_quarantine: Option<String>,
    log_hub: Option<LogHub>,
    hook_ingress: http_hooks::HookIngress,
    shutdown: Arc<AtomicBool>,
    teardown_complete: bool,
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        if !self.teardown_complete {
            self.registry.shutdown();
        }
    }
}

impl DaemonServer {
    pub fn bind() -> Result<Self, IpcError> {
        Self::bind_with_log_hub(None)
    }

    pub fn bind_with_log_hub(log_hub: Option<LogHub>) -> Result<Self, IpcError> {
        let admission = AdmissionConfig::from_environment().map_err(IpcError::Configuration)?;
        let (registry, store_writer, store_quarantine) = match persistence::default_path() {
            Some(path) => {
                let loaded = persistence::load(&path).map_err(IpcError::Store)?;
                let registry =
                    SessionRegistry::from_store_with_admission(loaded.snapshot, admission);
                // The disk tier lives beside the store, not inside it: the
                // store is rewritten whole on a debounce, and history is
                // appended to. Attached before the pipe is bound, so no session
                // can produce output that misses it.
                match persistence::scrollback_directory(&path)
                    .ok_or_else(|| "no data directory".to_owned())
                    .and_then(|directory| {
                        ScrollbackSpool::new(directory).map_err(|error| error.to_string())
                    }) {
                    Ok(spool) => registry.set_scrollback_spool(Arc::new(spool)),
                    Err(error) => {
                        // Losing history is a degradation, not a reason to
                        // refuse to supervise agents.
                        tracing::warn!(%error, "scrollback history is memory-only this run");
                    }
                }
                if let Some(worktrees) = persistence::worktree_directory(&path) {
                    registry.set_worktree_root(worktrees);
                }
                (
                    registry.clone(),
                    Some(StoreWriter::spawn(path, registry)),
                    loaded
                        .quarantined_path
                        .map(|path| path.to_string_lossy().into_owned()),
                )
            }
            None => (SessionRegistry::with_admission(admission), None, None),
        };
        Self::bind_named_with_state(PIPE_NAME, registry, store_writer, store_quarantine, log_hub)
    }

    pub fn bind_named(name: &str) -> Result<Self, IpcError> {
        Self::bind_named_with_state(name, SessionRegistry::new(), None, None, None)
    }

    fn bind_named_with_state(
        name: &str,
        registry: SessionRegistry,
        store_writer: Option<StoreWriter>,
        store_quarantine: Option<String>,
        log_hub: Option<LogHub>,
    ) -> Result<Self, IpcError> {
        // Parse the shared catalog during daemon startup so a malformed
        // resource cannot leave the Rust and web message layers silently out
        // of sync. The web renderer formats the same source for the operator.
        let _localization = terminalai_core::default_catalog()
            .map_err(|error| IpcError::Configuration(error.to_string()))?;
        let name = socket_name(name)?;
        let mut options = ListenerOptions::new().name(name);
        #[cfg(windows)]
        {
            use interprocess::os::windows::local_socket::ListenerOptionsExt;
            options = options.security_descriptor(current_user_pipe_descriptor()?);
        }
        // Interprocess sets FILE_FLAG_FIRST_PIPE_INSTANCE for the initial
        // Windows named-pipe instance. Disable name reclamation as well so a
        // second daemon cannot replace the first one on Unix or in a future
        // alternate local-socket backend. Remote clients remain rejected by
        // default; the DACL below narrows local access to SYSTEM and the owner.
        options = options.reclaim_name(false).try_overwrite(false);
        let listener = options.create_sync()?;
        let hook_ingress = http_hooks::HookIngress::bind(registry.clone()).map_err(IpcError::Io)?;
        Ok(Self {
            listener,
            registry,
            store_bridge: None,
            store_writer,
            store_quarantine,
            log_hub,
            hook_ingress,
            shutdown: Arc::new(AtomicBool::new(false)),
            teardown_complete: false,
        })
    }

    pub fn hook_endpoint(&self) -> HookEndpoint {
        self.hook_ingress.endpoint()
    }

    pub fn with_registry(name: &str, registry: SessionRegistry) -> Result<Self, IpcError> {
        Self::bind_named_with_state(name, registry, None, None, None)
    }

    /// Serve connections until a client requests shutdown or the console
    /// handler marks the process for teardown.
    pub fn serve(mut self) -> Result<(), IpcError> {
        if let Some(writer) = self.store_writer.clone() {
            self.store_bridge = Some(bridge_store(self.registry.clone(), writer));
        }
        spawn_transcript_poller(self.registry.clone(), self.shutdown.clone());
        let result = (|| {
            self.listener
                .set_nonblocking(ListenerNonblockingMode::Accept)?;
            let shutdown = self.shutdown.clone();
            let hook_endpoint = self.hook_endpoint();
            loop {
                if shutdown.load(Ordering::Acquire) || console_shutdown_requested() {
                    break;
                }
                let connection = match self.listener.incoming().next() {
                    Some(Ok(stream)) => stream,
                    Some(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    Some(Err(error)) => {
                        if shutdown.load(Ordering::Acquire) || console_shutdown_requested() {
                            break;
                        }
                        eprintln!("terminalai-daemon accept: {error}");
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    None => break,
                };
                let registry = self.registry.clone();
                let store_quarantine = self.store_quarantine.clone();
                let log_hub = self.log_hub.clone();
                let shutdown = shutdown.clone();
                let hook_endpoint = hook_endpoint.clone();
                // A transient spawn failure used to end serve() outright,
                // abandoning every live agent with no UI. One refused
                // client is not a reason to drop the fleet.
                if let Err(error) = thread::Builder::new()
                    .name("terminalai-daemon-client".into())
                    .spawn(move || {
                        if let Err(error) =
                            handle_connection(
                                connection,
                                registry,
                                store_quarantine,
                                log_hub,
                                shutdown,
                                hook_endpoint,
                            )
                        {
                            eprintln!("terminalai-daemon client: {error}");
                        }
                    })
                {
                    eprintln!(
                        "terminalai-daemon: could not start a client thread, dropping this connection: {error}"
                    );
                }
            }
            Ok::<(), IpcError>(())
        })();
        self.finish_shutdown();
        result
    }

    fn finish_shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(bridge) = self.store_bridge.as_mut() {
            bridge.stop();
        }
        let store_path = self
            .store_writer
            .as_ref()
            .map(|writer| writer.path().to_path_buf());
        if let Some(writer) = self.store_writer.take() {
            drop(writer);
        }

        // The asynchronous writer is stopped before teardown so an older
        // snapshot cannot race the synchronous final write below.
        self.registry.shutdown();
        if let Some(path) = store_path {
            if let Err(error) = self.registry.store_snapshot().write(&path) {
                eprintln!("terminalai-daemon: could not persist final session store: {error}");
            }
        }
        self.teardown_complete = true;
    }

    /// Test and embedding hook: handle one client and return when it closes.
    pub fn serve_one(&self) -> Result<(), IpcError> {
        let stream = self
            .listener
            .incoming()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "listener closed"))??;
        handle_connection(
            stream,
            self.registry.clone(),
            self.store_quarantine.clone(),
            self.log_hub.clone(),
            self.shutdown.clone(),
            self.hook_endpoint(),
        )
    }
}

fn bridge_store(registry: SessionRegistry, writer: StoreWriter) -> StoreBridge {
    let events = registry.subscribe();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = thread::Builder::new()
        .name("terminalai-session-store-events".into())
        .spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match events.recv_timeout(Duration::from_millis(25)) {
                    Ok(RegistryEvent::SessionUpdated { .. } | RegistryEvent::SessionRemoved { .. }) => {
                        writer.update();
                    }
                    Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    StoreBridge {
        stop,
        worker: worker.ok(),
    }
}

pub fn run() -> Result<(), IpcError> {
    run_with_log_hub(None)
}

pub fn run_with_log_hub(log_hub: Option<LogHub>) -> Result<(), IpcError> {
    install_panic_hook();
    install_console_handler();
    let server = match DaemonServer::bind_with_log_hub(log_hub) {
        Ok(server) => server,
        Err(error) => {
            signal_console_teardown_complete();
            return Err(error);
        }
    };
    tracing::info!(pipe = PIPE_NAME, "daemon control plane ready");
    let result = server.serve();
    signal_console_teardown_complete();
    result
}

#[cfg(windows)]
static CONSOLE_SHUTDOWN: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
static CONSOLE_TEARDOWN_COMPLETE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();

#[cfg(windows)]
fn console_teardown_latch() -> &'static (Mutex<bool>, Condvar) {
    CONSOLE_TEARDOWN_COMPLETE.get_or_init(|| (Mutex::new(false), Condvar::new()))
}

#[cfg(windows)]
fn signal_console_teardown_complete() {
    let (complete, wake) = console_teardown_latch();
    let mut complete = complete
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *complete = true;
    wake.notify_all();
}

#[cfg(not(windows))]
fn signal_console_teardown_complete() {}

#[cfg(windows)]
fn install_console_handler() {
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };

    CONSOLE_SHUTDOWN.store(false, Ordering::Release);
    let (complete, _) = console_teardown_latch();
    *complete
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
    let installed = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) };
    if installed == 0 {
        // The packaged daemon is normally launched without a console. That is
        // expected; named-pipe shutdown remains available in that mode.
        eprintln!(
            "terminalai-daemon: console shutdown handler unavailable; use `terminalai-probe shutdown`"
        );
    }

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
        if matches!(
            ctrl_type,
            CTRL_C_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
        ) {
            CONSOLE_SHUTDOWN.store(true, Ordering::Release);
            if matches!(ctrl_type, CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT) {
                let (complete, wake) = console_teardown_latch();
                let mut complete = complete
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                while !*complete {
                    complete = wake
                        .wait(complete)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
            1
        } else {
            0
        }
    }
}

#[cfg(not(windows))]
fn install_console_handler() {}

#[cfg(windows)]
fn console_shutdown_requested() -> bool {
    CONSOLE_SHUTDOWN.load(Ordering::Acquire)
}

#[cfg(not(windows))]
fn console_shutdown_requested() -> bool {
    false
}

fn handle_connection(
    stream: LocalSocketStream,
    registry: SessionRegistry,
    store_quarantine: Option<String>,
    log_hub: Option<LogHub>,
    shutdown: Arc<AtomicBool>,
    hook_endpoint: HookEndpoint,
) -> Result<(), IpcError> {
    let peer_pid = stream.peer_creds()?.pid();
    tracing::debug!(peer_pid = ?peer_pid, "control connection accepted");
    let (receive, send) = stream.split();
    let (outgoing_tx, outgoing_rx) = mpsc::sync_channel::<WireMessage>(OUTGOING_QUEUE_CAPACITY);
    let writer = thread::Builder::new()
        .name("terminalai-daemon-writer".into())
        .spawn(move || write_messages(send, outgoing_rx))
        .map_err(IpcError::Io)?;

    let mut reader = BufReader::new(receive);
    let mut authenticated = false;
    let mut subscribed = false;
    let mut line = String::new();
    let (event_stop_tx, event_stop_rx) = mpsc::channel();
    let mut event_stop_rx = Some(event_stop_rx);
    let mut event_thread = None;
    let result = (|| -> Result<(), IpcError> {
        loop {
            match read_frame(&mut reader, &mut line) {
                Ok(0) => break,
                Ok(_) => {}
                // An oversized frame is the peer's fault, not ours, and the
                // connection may still carry sane traffic afterwards only if the
                // stream is resynchronized - which it cannot be mid-frame. Say
                // why and close.
                Err(error) => return Err(error),
            }
            let message = match serde_json::from_str::<WireMessage>(&line) {
                Ok(message) => message,
                // A malformed frame used to tear down the connection with no
                // reply, so a client saw a disconnect where it had made a
                // mistake. Answer and keep serving.
                Err(error) => {
                    send_response(
                        &outgoing_tx,
                        0,
                        Response::Error {
                            message: format!("malformed control frame: {error}"),
                        },
                    )?;
                    continue;
                }
            };
            let WireMessage::Request { id, request } = message else {
                send_response(
                    &outgoing_tx,
                    0,
                    Response::Error {
                        message: "client messages must be requests".into(),
                    },
                )?;
                continue;
            };

            if !authenticated {
                let Request::Hello {
                    protocol,
                    client_pid,
                } = request
                else {
                    send_response(
                        &outgoing_tx,
                        id,
                        Response::Error {
                            message: "Hello must be the first request".into(),
                        },
                    )?;
                    break;
                };
                if protocol != PROTOCOL_VERSION {
                    tracing::warn!(
                        client_protocol = protocol,
                        daemon_protocol = PROTOCOL_VERSION,
                        "control protocol mismatch"
                    );
                    send_response(
                        &outgoing_tx,
                        id,
                        // Keep the daemon identity in a typed handshake so a
                        // newer client can explain the skew and refuse to
                        // launch a second daemon over the live one.
                        Response::Hello {
                            protocol: PROTOCOL_VERSION,
                            daemon_pid: std::process::id(),
                        },
                    )?;
                    break;
                }
                // The DACL already authorized this process. Keep the PID
                // comparison as a diagnostic defense-in-depth check, not as
                // the security boundary.
                if peer_pid != Some(client_pid) {
                    send_response(
                        &outgoing_tx,
                        id,
                        Response::Error {
                            message: IpcError::PeerMismatch {
                                expected: client_pid,
                                actual: peer_pid,
                            }
                            .to_string(),
                        },
                    )?;
                    break;
                }
                authenticated = true;
                tracing::info!(peer_pid = ?peer_pid, "control client authenticated");
                send_response(
                    &outgoing_tx,
                    id,
                    Response::Hello {
                        protocol: PROTOCOL_VERSION,
                        daemon_pid: std::process::id(),
                    },
                )?;
                continue;
            }

            match request {
                Request::Close => {
                    send_response(&outgoing_tx, id, Response::Ok)?;
                    break;
                }
                Request::Shutdown => {
                    tracing::info!("daemon shutdown requested");
                    send_response(&outgoing_tx, id, Response::Ok)?;
                    shutdown.store(true, Ordering::Release);
                    break;
                }
                Request::Subscribe => {
                    if !subscribed {
                        tracing::debug!("control client subscribed to events");
                        let stop = event_stop_rx
                            .take()
                            .expect("event stop receiver only used once");
                        event_thread = Some(bridge_events(
                            registry.subscribe(),
                            outgoing_tx.clone(),
                            registry.clone(),
                            log_hub.as_ref().map(LogHub::subscribe),
                            stop,
                        )?);
                        subscribed = true;
                    }
                    send_response(&outgoing_tx, id, Response::Ok)?;
                }
                request => {
                    let response = dispatch_with_endpoint(
                        request,
                        &registry,
                        store_quarantine.as_deref(),
                        Some(&hook_endpoint),
                    );
                    send_response(&outgoing_tx, id, response)?;
                }
            }
        }
        Ok(())
    })();
    drop(outgoing_tx);
    let _ = event_stop_tx.send(());
    if let Some(event_thread) = event_thread {
        event_thread
            .join()
            .map_err(|_| IpcError::Io(io::Error::other("event thread panicked")))?;
    }
    let writer_result = writer
        .join()
        .map_err(|_| IpcError::Io(io::Error::other("writer thread panicked")))?;
    result?;
    writer_result?;
    Ok(())
}

/// How often each agent is asked whether it is still authenticated.
const AUTH_PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Ask each installed agent about its credentials and record the answer.
///
/// An agent that cannot be resolved is not probed at all: "not installed" is
/// already reported by preflight, and reporting it as an auth failure would put
/// a banner in front of the operator that logging in cannot clear.
fn refresh_agent_auth(registry: &SessionRegistry) {
    for agent in [Agent::Claude, Agent::Codex] {
        let Ok(binary) = terminalai_core::agent::resolve(agent, None) else {
            continue;
        };
        let auth = terminalai_core::auth::probe(agent, &binary.path);
        if registry.set_agent_auth(auth.clone()) {
            tracing::info!(
                agent = agent.command_name(),
                state = ?auth.state,
                "agent authentication state changed"
            );
        }
    }
}

/// The daemon-wide policy in the units the settings dialog edits.
///
/// Megabytes and hours rather than bytes and durations: the dialog is where an
/// operator types a number, and converting in one place keeps the two ends from
/// disagreeing about which unit a field is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmissionSettings {
    pub max_live_sessions: usize,
    pub default_budget_usd: Option<f64>,
    pub spend_ceiling_usd: Option<f64>,
    pub spend_window_hours: f64,
    pub memory_budget_mb: Option<u64>,
    pub session_memory_cap_mb: Option<u64>,
    pub max_processes_per_session: Option<u32>,
    /// Environment variables that were set when the daemon started. The dialog
    /// says so, because a value an operator did not type here came from
    /// somewhere and silently overriding it would be the confusing half of
    /// having two sources.
    pub from_environment: Vec<String>,
}

/// The environment variables that seed the admission policy at boot.
const ADMISSION_ENVIRONMENT: &[&str] = &[
    "TERMINALAI_MAX_LIVE_SESSIONS",
    "TERMINALAI_DEFAULT_BUDGET_USD",
    "TERMINALAI_SPEND_CEILING_USD",
    "TERMINALAI_SPEND_WINDOW_HOURS",
    "TERMINALAI_MEMORY_BUDGET_MB",
    "TERMINALAI_SESSION_MEMORY_CAP_MB",
    "TERMINALAI_MAX_PROCESSES_PER_SESSION",
];

fn admission_settings(
    config: &terminalai_core::registry::AdmissionConfig,
) -> AdmissionSettings {
    AdmissionSettings {
        max_live_sessions: config.max_live_sessions,
        default_budget_usd: config.default_budget_usd,
        spend_ceiling_usd: config.spend_ceiling_usd,
        spend_window_hours: config.spend_window.as_secs_f64() / 3600.0,
        memory_budget_mb: config.memory_budget_bytes.map(|bytes| bytes / (1024 * 1024)),
        session_memory_cap_mb: config
            .session_memory_cap_bytes
            .map(|bytes| bytes / (1024 * 1024)),
        max_processes_per_session: config.max_processes_per_session,
        from_environment: ADMISSION_ENVIRONMENT
            .iter()
            .filter(|name| std::env::var_os(name).is_some())
            .map(|name| (*name).to_owned())
            .collect(),
    }
}

fn write_messages(mut send: SendHalf, outgoing: Receiver<WireMessage>) -> Result<(), IpcError> {
    for message in outgoing {
        let mut encoded = serde_json::to_vec(&message)?;
        encoded.push(b'\n');
        send.write_all(&encoded)?;
    }
    Ok(())
}

fn send_response(
    outgoing: &SyncSender<WireMessage>,
    id: u64,
    response: Response,
) -> Result<(), IpcError> {
    outgoing
        .send(WireMessage::Response {
            id,
            response: Box::new(response),
        })
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client disconnected").into())
}

fn bridge_events(
    events: Receiver<RegistryEvent>,
    outgoing: SyncSender<WireMessage>,
    registry: SessionRegistry,
    mut logs: Option<Receiver<LogEntry>>,
    stop: Receiver<()>,
) -> Result<thread::JoinHandle<()>, IpcError> {
    thread::Builder::new()
        .name("terminalai-daemon-events".into())
        .spawn(move || loop {
            if !drain_log_events(&mut logs, &outgoing, &registry) {
                break;
            }
            match stop.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match events.recv_timeout(EVENT_POLL_INTERVAL) {
                Ok(event) => match outgoing.try_send(WireMessage::Event { event }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => registry.record_dropped_event(),
                    Err(TrySendError::Disconnected(_)) => break,
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        })
        .map_err(IpcError::Io)
}

fn drain_log_events(
    logs: &mut Option<Receiver<LogEntry>>,
    outgoing: &SyncSender<WireMessage>,
    registry: &SessionRegistry,
) -> bool {
    let Some(logs) = logs.as_ref() else {
        return true;
    };
    loop {
        match logs.try_recv() {
            Ok(entry) => match outgoing.try_send(WireMessage::Event {
                event: RegistryEvent::Log { entry },
            }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => registry.record_dropped_event(),
                Err(TrySendError::Disconnected(_)) => return false,
            },
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return true,
        }
    }
}

#[cfg(test)]
fn dispatch(request: Request, registry: &SessionRegistry) -> Response {
    dispatch_with_endpoint(request, registry, None, None)
}

#[cfg(test)]
fn dispatch_with_quarantine(
    request: Request,
    registry: &SessionRegistry,
    store_quarantine: Option<&str>,
) -> Response {
    dispatch_with_endpoint(request, registry, store_quarantine, None)
}

fn dispatch_with_endpoint(
    request: Request,
    registry: &SessionRegistry,
    store_quarantine: Option<&str>,
    hook_endpoint: Option<&HookEndpoint>,
) -> Response {
    if registry.is_poisoned() && request_requires_registry(&request) {
        return Response::Error {
            message: "registry lock is poisoned; stateful request refused".into(),
        };
    }
    match request {
        Request::Hello { .. } => Response::Error {
            message: "Hello was already completed".into(),
        },
        Request::Subscribe => Response::Error {
            message: "Subscribe is handled by the connection".into(),
        },
        Request::Close => Response::Ok,
        Request::Shutdown => Response::Ok,
        Request::Ping => Response::Pong,
        Request::Snapshot => Response::Snapshot {
            sessions: registry.snapshot(),
            focused: registry.focused(),
            admission: registry.admission_snapshot(),
            store_quarantine: store_quarantine.map(str::to_owned),
        },
        Request::ReviewSnapshot => Response::ReviewSnapshot {
            entries: registry.review_snapshot(),
        },
        Request::ExternalSessions => Response::ExternalSessions {
            sessions: external_sessions(),
        },
        Request::SessionHistory => Response::SessionHistory {
            archives: registry.archives(),
        },
        Request::Land { request } => Response::Land {
            outcome: land_queue().land(&request),
        },
        Request::AdmissionConfig => Response::Admission {
            admission: admission_settings(&registry.admission_config()),
        },
        Request::SetAdmission {
            max_live_sessions,
            default_budget_usd,
            spend_ceiling_usd,
            spend_window_hours,
            memory_budget_mb,
            session_memory_cap_mb,
            max_processes_per_session,
        } => {
            let config = terminalai_core::registry::AdmissionConfig::new(
                max_live_sessions,
                default_budget_usd,
            )
            .with_spend_ceiling(
                spend_ceiling_usd,
                spend_window_hours
                    .filter(|hours| hours.is_finite() && *hours > 0.0)
                    .map(|hours| Duration::from_secs_f64(hours * 3600.0)),
            )
            .with_memory_limits(
                memory_budget_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
                session_memory_cap_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
                max_processes_per_session,
            );
            registry.set_admission(config);
            Response::Admission {
                admission: admission_settings(&registry.admission_config()),
            }
        }
        Request::MarkReviewed { id } => match registry.mark_reviewed(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Status { id } => match registry
            .snapshot()
            .into_iter()
            .find(|session| session.id == id)
        {
            Some(session) => Response::Status {
                session,
                admission: registry.admission_snapshot(),
            },
            None => Response::Error {
                message: format!("session does not exist: {id}"),
            },
        },
        Request::Resolve {
            agent,
            configured_path,
        } => match agent::resolve(agent, configured_path.as_deref()) {
            Ok(binary) => Response::Resolved {
                agent: binary.agent,
                path: binary.path,
                origin: origin_label(binary.origin).into(),
            },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Capabilities {
            agent,
            configured_path,
        } => match terminalai_core::probe_capabilities(agent, configured_path.as_deref()) {
            Ok(capabilities) => Response::Capabilities { capabilities },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Preview {
            spec,
            configured_path,
        } => {
            let spec = *spec;
            warn_capability_overrides(&spec, configured_path.as_deref());
            match agent::resolve(spec.agent, configured_path.as_deref()) {
                Ok(binary) => match spec.resolve(&binary) {
                    Ok(command) => Response::Preview {
                        command: command.preview(),
                    },
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                },
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::HookEndpoint => match hook_endpoint {
            Some(endpoint) => Response::HookEndpoint {
                endpoint: endpoint.clone(),
            },
            None => Response::Error {
                message: "HTTP hook endpoint is unavailable".into(),
            },
        },
        Request::Hook { event, hook_token } => Response::Hook {
            matched: registry.apply_hook_with_token(event, hook_token.as_deref()),
        },
        Request::AgentEvent { event } => Response::AgentEvent {
            matched: registry.apply_agent_event(event),
        },
        Request::Launch {
            spec,
            configured_path,
        } => {
            let spec = *spec;
            warn_capability_overrides(&spec, configured_path.as_deref());
            match agent::resolve(spec.agent, configured_path.as_deref()) {
                Ok(binary) => match registry.launch(spec, binary) {
                    Ok(id) => match registry.is_queued(&id) {
                        Ok(queued) => Response::Launched { id, queued },
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Err(error) => Response::Error {
                        message: resolve_registry_error(error),
                    },
                },
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::Write { id, data } => {
            // Rejected whole, never truncated: half a prompt reaching an agent is
            // worse than none, because the agent would act on the fragment.
            if data.len() > MAX_WRITE_BYTES {
                Response::Error {
                    message: format!(
                        "write payload of {} bytes exceeds the {MAX_WRITE_BYTES}-byte limit",
                        data.len()
                    ),
                }
            } else {
                match registry.write_user_input(&id, data.as_bytes()) {
                    Ok(()) => Response::Ok,
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                }
            }
        }
        Request::QueuedPrompts { id } => match registry.queued_prompts(&id) {
            Ok(prompts) => Response::QueuedPrompts { prompts },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::EnqueuePrompt { id, text } => {
            if text.len() > MAX_WRITE_BYTES {
                Response::Error {
                    message: format!(
                        "queued prompt of {} bytes exceeds the {MAX_WRITE_BYTES}-byte limit",
                        text.len()
                    ),
                }
            } else {
                match registry.enqueue_prompt(&id, &text) {
                    Ok(prompt) => Response::Enqueued { prompt },
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                }
            }
        }
        Request::EditQueuedPrompt { id, prompt, text } => {
            match registry.edit_queued_prompt(&id, prompt, &text) {
                Ok(()) => Response::Ok,
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::RemoveQueuedPrompt { id, prompt } => {
            match registry.remove_queued_prompt(&id, prompt) {
                Ok(()) => Response::Ok,
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::ReorderQueuedPrompt { id, prompt, to } => {
            match registry.reorder_queued_prompt(&id, prompt, to) {
                Ok(()) => Response::Ok,
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::PauseQueue { id } => match registry.pause_queue(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::ResumeQueue { id } => match registry.resume_queue(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Broadcast { ids, data } => {
            if data.len() > MAX_WRITE_BYTES {
                Response::Error {
                    message: format!(
                        "broadcast payload of {} bytes exceeds the {MAX_WRITE_BYTES}-byte limit",
                        data.len()
                    ),
                }
            } else if ids.len() > MAX_BROADCAST_TARGETS {
                Response::Error {
                    message: format!(
                        "broadcast to {} sessions exceeds the {MAX_BROADCAST_TARGETS}-session limit",
                        ids.len()
                    ),
                }
            } else {
                Response::Broadcast {
                    results: registry.broadcast(&ids, data.as_bytes()),
                }
            }
        }
        Request::Resize {
            id,
            rows,
            cols,
            pixel_width,
            pixel_height,
        } => match registry.resize(
            &id,
            PtySize {
                rows,
                cols,
                pixel_width,
                pixel_height,
            },
        ) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Kill { id } => match registry.kill(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Focus { id } => match registry.focus(id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::MarkRead { id } => match registry.mark_read(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::TogglePin { id } => match registry.toggle_pin(&id) {
            Ok(pinned) => Response::PinChanged { pinned },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::GridSnapshot { id } => match registry.grid_snapshot(&id) {
            Ok(grid) => Response::GridSnapshot { grid },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Scrollback { id } => match registry.scrollback(&id) {
            Ok(data) => Response::Scrollback { data },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::ScrollbackHistory { id, max_bytes } => {
            match registry.scrollback_history(&id, max_bytes.min(MAX_HISTORY_BYTES)) {
                Ok(data) => Response::ScrollbackHistory { data },
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::Reattach { id } => match registry.reattach(&id) {
            Ok(data) => Response::Reattached { data },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Revive { id } => match registry.revive(&id) {
            Ok(id) => Response::Revived { id },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Archive { id } => match registry.archive(&id) {
            Ok(archive) => Response::Archived { id: archive.id },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
    }
}

fn request_requires_registry(request: &Request) -> bool {
    !matches!(
        request,
        Request::Hello { .. }
            | Request::Subscribe
            | Request::Ping
            | Request::Close
            | Request::Shutdown
            | Request::Resolve { .. }
            | Request::Capabilities { .. }
            | Request::Preview { .. }
            | Request::HookEndpoint
    )
}

fn warn_capability_overrides(spec: &LaunchSpec, configured_path: Option<&std::path::Path>) {
    let Ok(capabilities) = terminalai_core::probe_capabilities(spec.agent, configured_path) else {
        tracing::debug!(agent = ?spec.agent, "runtime capability probe unavailable; launch remains permissive");
        return;
    };
    for warning in capabilities.warnings_for(spec.model.as_deref(), spec.effort.as_ref()) {
        tracing::warn!(agent = ?spec.agent, warning = %warning, "launch value is outside detected capabilities");
    }
}

fn resolve_registry_error(error: terminalai_core::RegistryError) -> String {
    error.to_string()
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::Configured => "configured",
        Origin::NpmPrefix => "npm-prefix",
        Origin::Path => "path",
    }
}

#[derive(Clone)]
pub struct DaemonClient {
    writer: Arc<Mutex<SendHalf>>,
    pending: Arc<Mutex<HashMap<u64, Sender<Response>>>>,
    events: Arc<Mutex<Receiver<RegistryEvent>>>,
    next_id: Arc<AtomicU64>,
}

impl DaemonClient {
    pub fn connect() -> Result<Self, IpcError> {
        Self::connect_with_timeout(Duration::from_secs(30))
    }

    /// Connect to the stable endpoint, falling back once to the v2 endpoint
    /// so an upgrade can still attach to a daemon from the pre-stable-name
    /// release instead of starting a second owner of the fleet.
    pub fn connect_with_timeout(timeout: Duration) -> Result<Self, IpcError> {
        match Self::connect_named_with_timeout(PIPE_NAME, timeout) {
            Ok(client) => Ok(client),
            Err(error) if !error.is_version_mismatch() => {
                Self::connect_named_with_timeout(LEGACY_PIPE_NAME, timeout).or(Err(error))
            }
            Err(error) => Err(error),
        }
    }

    pub fn connect_named(name: &str) -> Result<Self, IpcError> {
        Self::connect_named_with_timeout(name, Duration::from_secs(30))
    }

    pub fn connect_named_with_timeout(name: &str, timeout: Duration) -> Result<Self, IpcError> {
        let stream = LocalSocketStream::connect(socket_name(name)?)?;
        let (receive, send) = stream.split();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = mpsc::channel();
        spawn_reader(receive, pending.clone(), event_tx)?;
        let client = Self {
            writer: Arc::new(Mutex::new(send)),
            pending,
            events: Arc::new(Mutex::new(event_rx)),
            next_id: Arc::new(AtomicU64::new(1)),
        };
        match client.call_with_timeout(
            Request::Hello {
                protocol: PROTOCOL_VERSION,
                client_pid: std::process::id(),
            },
            timeout,
        )? {
            Response::Hello { protocol, .. } if protocol == PROTOCOL_VERSION => Ok(client),
            Response::Hello {
                protocol,
                daemon_pid,
            } => Err(IpcError::VersionMismatch {
                daemon: protocol,
                client: PROTOCOL_VERSION,
                daemon_pid,
            }),
            Response::Error { message } => Err(IpcError::Remote(message)),
            other => Err(IpcError::InvalidMessage(format!(
                "unexpected hello response: {other:?}"
            ))),
        }
    }

    pub fn call(&self, request: Request) -> Result<Response, IpcError> {
        self.call_with_timeout(request, Duration::from_secs(30))
    }

    pub fn call_with_timeout(
        &self,
        request: Request,
        timeout: Duration,
    ) -> Result<Response, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| IpcError::Poisoned("pending requests"))?
            .insert(id, sender);
        if let Err(error) = self.send(WireMessage::Request { id, request }) {
            let _ = self
                .pending
                .lock()
                .map_err(|_| IpcError::Poisoned("pending requests"))?
                .remove(&id);
            return Err(error);
        }
        receiver.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => IpcError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => IpcError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "daemon disconnected",
            )),
        })
    }

    pub fn subscribe(&self) -> Result<(), IpcError> {
        match self.call(Request::Subscribe)? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(IpcError::Remote(message)),
            other => Err(IpcError::InvalidMessage(format!(
                "unexpected subscribe response: {other:?}"
            ))),
        }
    }

    pub fn shutdown(&self) -> Result<(), IpcError> {
        match self.call(Request::Shutdown)? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(IpcError::Remote(message)),
            other => Err(IpcError::InvalidMessage(format!(
                "unexpected shutdown response: {other:?}"
            ))),
        }
    }

    pub fn hook_endpoint(&self) -> Result<HookEndpoint, IpcError> {
        match self.call(Request::HookEndpoint)? {
            Response::HookEndpoint { endpoint } => Ok(endpoint),
            Response::Error { message } => Err(IpcError::Remote(message)),
            other => Err(IpcError::InvalidMessage(format!(
                "unexpected hook endpoint response: {other:?}"
            ))),
        }
    }

    pub fn events(&self) -> Arc<Mutex<Receiver<RegistryEvent>>> {
        self.events.clone()
    }

    fn send(&self, message: WireMessage) -> Result<(), IpcError> {
        let mut encoded = serde_json::to_vec(&message)?;
        encoded.push(b'\n');
        self.writer
            .lock()
            .map_err(|_| IpcError::Poisoned("control writer"))?
            .write_all(&encoded)
            .map_err(IpcError::Io)
    }
}

fn spawn_reader(
    receive: RecvHalf,
    pending: Arc<Mutex<HashMap<u64, Sender<Response>>>>,
    events: Sender<RegistryEvent>,
) -> Result<(), IpcError> {
    thread::Builder::new()
        .name("terminalai-daemon-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(receive);
            let mut line = String::new();
            // Ends on EOF or on any framing error: an oversized frame leaves the
            // stream desynchronized, and the daemon should never send one anyway.
            while let Ok(read) = read_frame(&mut reader, &mut line) {
                if read == 0 {
                    break;
                }
                let Ok(message) = serde_json::from_str::<WireMessage>(&line) else {
                    continue;
                };
                match message {
                    WireMessage::Response { id, response } => {
                        let response = *response;
                        if let Ok(mut waiting) = pending.lock() {
                            if let Some(sender) = waiting.remove(&id) {
                                let _ = sender.send(response);
                            }
                        }
                    }
                    WireMessage::Event { event } => {
                        let _ = events.send(event);
                    }
                    WireMessage::Request { .. } => {}
                }
            }
            if let Ok(mut waiting) = pending.lock() {
                for (_, sender) in waiting.drain() {
                    let _ = sender.send(Response::Error {
                        message: "daemon disconnected".into(),
                    });
                }
            }
        })
        .map(|_| ())
        .map_err(IpcError::Io)
}

fn socket_name(name: &str) -> Result<Name<'static>, IpcError> {
    name.to_ns_name::<GenericNamespaced>()
        .map(Name::into_owned)
        .map_err(IpcError::Io)
}

#[cfg(windows)]
fn current_user_pipe_descriptor(
) -> Result<interprocess::os::windows::security_descriptor::SecurityDescriptor, IpcError> {
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;

    let sddl = U16CString::from_str(&current_user_pipe_sddl()?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    SecurityDescriptor::deserialize(sddl.as_ucstr()).map_err(IpcError::Io)
}

#[cfg(windows)]
fn current_user_pipe_sddl() -> Result<String, IpcError> {
    use std::ptr::null_mut;
    use widestring::U16CStr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = null_mut();
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 {
        return Err(IpcError::Io(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32,
        )));
    }

    let result = (|| {
        let mut bytes = 0u32;
        unsafe {
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(IpcError::Io(io::Error::last_os_error()));
        }
        let mut buffer = vec![0u8; bytes as usize];
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        };
        if queried == 0 {
            return Err(IpcError::Io(io::Error::last_os_error()));
        }
        let token_user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
        let mut sid_string = null_mut();
        let converted = unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid_string) };
        if converted == 0 {
            return Err(IpcError::Io(io::Error::last_os_error()));
        }
        let sid = unsafe { U16CStr::from_ptr_str(sid_string) }.to_string_lossy();
        unsafe {
            LocalFree(sid_string.cast());
        }
        if !sid.starts_with("S-") {
            return Err(IpcError::InvalidMessage(
                "current token did not yield a Windows SID".into(),
            ));
        }
        Ok(format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})"))
    })();
    unsafe {
        CloseHandle(token);
    }
    result
}

/// The process-wide landing queue.
///
/// Deliberately a singleton: a per-connection queue would let two clients land
/// at once, which is exactly the interleaving the gate exists to prevent.
fn land_queue() -> &'static LandQueue {
    static QUEUE: OnceLock<LandQueue> = OnceLock::new();
    QUEUE.get_or_init(LandQueue::new)
}

/// How often each live session's transcript is re-read.
///
/// Both CLIs append continuously during a turn, so a filesystem watcher would
/// fire hundreds of times per response for the same three fields. Two seconds
/// is well under the time it takes an operator to look at a row and far above
/// the rate at which a re-read would cost anything: each poll reads only the
/// bytes appended since the last one.
const TRANSCRIPT_POLL_INTERVAL: Duration = Duration::from_secs(2);

fn spawn_transcript_poller(registry: SessionRegistry, shutdown: Arc<AtomicBool>) {
    let Some(home) = dirs::home_dir() else {
        // Without a home directory there is nowhere to look. Say so once rather
        // than starting a thread that can never find anything.
        eprintln!("terminalai-daemon: no home directory; transcript tailing is disabled");
        return;
    };
    let spawned = thread::Builder::new()
        .name("terminalai-transcripts".to_owned())
        .spawn(move || {
            // Probe on the first pass so an already-expired login is reported
            // before the operator queues work against it.
            let mut last_auth_probe = Instant::now() - AUTH_PROBE_INTERVAL;
            while !shutdown.load(Ordering::Acquire) {
                registry.poll_transcripts(&home);
                // Same wakeup rather than a second timer: memory does not move
                // fast enough to justify one, and the fleet already pays for
                // this one.
                registry.sample_memory();
                // Far slower than the transcript poll: credentials change on the
                // scale of hours, and each probe is a process spawn per agent.
                if last_auth_probe.elapsed() >= AUTH_PROBE_INTERVAL {
                    last_auth_probe = Instant::now();
                    refresh_agent_auth(&registry);
                }
                thread::sleep(TRANSCRIPT_POLL_INTERVAL);
            }
        });
    if let Err(error) = spawned {
        // Cost and resume ids simply will not appear. That is a visible
        // degradation, so it is reported rather than left to be inferred.
        eprintln!("terminalai-daemon: transcript tailing unavailable: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use terminalai_core::AppServerEvent;
    use terminalai_core::{
        Agent, LaunchSpec, ResolvedCommand, Session, SessionId, SessionStoreSnapshot,
        StoredSession, SESSION_STORE_MAGIC, SESSION_STORE_SCHEMA_VERSION,
    };

    #[test]
    fn history_budget_reaches_before_the_memory_ring() {
        assert!(
            MAX_HISTORY_BYTES > terminalai_core::registry::MAX_SCROLLBACK_BYTES as u64,
            "history must include bytes older than the in-memory ring"
        );
        assert!(
            MAX_FRAME_BYTES >= (MAX_HISTORY_BYTES as usize).saturating_mul(4),
            "JSON byte arrays need several times their raw history size"
        );
    }

    #[test]
    fn readable_empty_external_registry_does_not_invoke_cli_fallback() {
        let home = std::env::temp_dir().join(format!(
            "terminalai-daemon-external-empty-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".claude").join("sessions"))
            .expect("create readable empty registry");
        let fallback_called = Arc::new(AtomicBool::new(false));
        let marker = fallback_called.clone();

        let sessions = external_sessions_from(&home, move || {
            marker.store(true, Ordering::Relaxed);
            Vec::new()
        });

        assert!(sessions.is_empty());
        assert!(
            !fallback_called.load(Ordering::Relaxed),
            "a readable empty registry must not spawn the CLI fallback"
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn an_endless_frame_is_refused_instead_of_exhausting_memory() {
        // A peer that never sends a newline used to grow a String until the
        // process died — and both ends of this protocol are programs.
        let flood = vec![b'x'; MAX_FRAME_BYTES + 4096];
        let mut reader = BufReader::new(io::Cursor::new(flood));
        let mut line = String::new();
        let error = read_frame(&mut reader, &mut line).expect_err("oversized frame");
        assert!(
            matches!(&error, IpcError::InvalidMessage(message) if message.contains("exceeded")),
            "expected a typed size error, got {error:?}"
        );
    }

    #[test]
    fn a_frame_at_the_limit_still_reads() {
        // The bound must reject what is over it and nothing else.
        let mut body = vec![b'x'; MAX_FRAME_BYTES - 1];
        body.push(b'\n');
        let mut reader = BufReader::new(io::Cursor::new(body));
        let mut line = String::new();
        assert_eq!(
            read_frame(&mut reader, &mut line).expect("frame at the limit"),
            MAX_FRAME_BYTES
        );
    }

    #[test]
    fn frames_are_read_one_at_a_time_and_eof_is_zero() {
        let mut reader = BufReader::new(io::Cursor::new(b"{\"a\":1}\n{\"b\":2}\n".to_vec()));
        let mut line = String::new();
        assert_eq!(read_frame(&mut reader, &mut line).unwrap(), 8);
        assert_eq!(line, "{\"a\":1}\n");
        assert_eq!(read_frame(&mut reader, &mut line).unwrap(), 8);
        assert_eq!(line, "{\"b\":2}\n");
        assert_eq!(read_frame(&mut reader, &mut line).unwrap(), 0);
    }

    #[test]
    fn an_oversized_write_is_refused_whole_and_never_truncated() {
        let registry = SessionRegistry::new();
        let response = dispatch(
            Request::Write {
                id: SessionId::new(1),
                data: "x".repeat(MAX_WRITE_BYTES + 1),
            },
            &registry,
        );
        match response {
            Response::Error { message } => {
                assert!(message.contains("exceeds"), "unhelpful refusal: {message}");
            }
            other => panic!("an oversized write must be refused, got {other:?}"),
        }
        // At the limit it is still the session lookup that decides, not the size.
        let response = dispatch(
            Request::Write {
                id: SessionId::new(1),
                data: "x".repeat(MAX_WRITE_BYTES),
            },
            &registry,
        );
        match response {
            Response::Error { message } => assert!(
                !message.contains("exceeds"),
                "a payload at the limit must not be refused for size: {message}"
            ),
            Response::Ok => {}
            other => panic!("unexpected response {other:?}"),
        }
    }

    #[test]
    fn request_envelope_is_tagged_and_round_trips() {
        let message = WireMessage::Request {
            id: 7,
            request: Request::Ping,
        };
        let json = serde_json::to_string(&message).expect("encode");
        assert!(json.contains("\"kind\":\"request\""));
        assert!(matches!(
            serde_json::from_str(&json),
            Ok(WireMessage::Request { id: 7, .. })
        ));
    }

    #[test]
    fn agent_event_request_is_an_additive_wire_variant() {
        let message = WireMessage::Request {
            id: 8,
            request: Request::AgentEvent {
                event: AgentEvent::AppServer(AppServerEvent::Unknown {
                    method: "future/event".into(),
                    params: serde_json::json!({"value": true}),
                }),
            },
        };
        let json = serde_json::to_string(&message).expect("encode");
        assert!(json.contains("agent_event"));
        assert!(matches!(
            serde_json::from_str(&json),
            Ok(WireMessage::Request {
                id: 8,
                request: Request::AgentEvent { .. }
            })
        ));
    }

    #[test]
    fn events_are_a_separate_wire_variant() {
        let event = WireMessage::Event {
            event: RegistryEvent::SessionRemoved {
                id: SessionId("s0001".into()),
            },
        };
        let json = serde_json::to_string(&event).expect("encode");
        assert!(json.contains("\"kind\":\"event\""));
        assert!(!json.contains("\"id\":0"));
    }

    #[test]
    fn dispatcher_handles_ping_without_a_session() {
        assert!(matches!(
            dispatch(Request::Ping, &SessionRegistry::new()),
            Response::Pong
        ));
    }

    #[test]
    fn snapshot_includes_the_quarantined_store_path() {
        assert!(matches!(
            dispatch_with_quarantine(
                Request::Snapshot,
                &SessionRegistry::new(),
                Some(r"C:\Users\me\sessions.corrupt-2026-08-02T12-34-56Z.json"),
            ),
            Response::Snapshot {
                store_quarantine: Some(path),
                ..
            } if path == r"C:\Users\me\sessions.corrupt-2026-08-02T12-34-56Z.json"
        ));
    }

    #[test]
    fn dispatcher_reports_a_missing_status_without_touching_the_registry() {
        assert!(matches!(
            dispatch(
                Request::Status {
                    id: SessionId("missing".into())
                },
                &SessionRegistry::new()
            ),
            Response::Error { message } if message.contains("session does not exist")
        ));
    }

    #[test]
    fn peer_handshake_round_trip_uses_the_named_socket() {
        let name = format!(
            "terminalai-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let server = DaemonServer::bind_named(&name).expect("bind test socket");
        let server_thread = thread::spawn(move || server.serve_one());
        let client = DaemonClient::connect_named(&name).expect("connect test socket");
        let endpoint = client.hook_endpoint().expect("hook endpoint");
        assert!(endpoint.base_url.starts_with("http://127.0.0.1:"));
        assert_eq!(
            endpoint.host,
            endpoint.base_url.trim_start_matches("http://")
        );
        assert_eq!(endpoint.bearer_token.len(), 64);
        assert!(matches!(client.call(Request::Ping), Ok(Response::Pong)));
        assert!(matches!(client.call(Request::Close), Ok(Response::Ok)));
        drop(client);
        server_thread
            .join()
            .expect("server thread")
            .expect("serve client");
    }

    #[test]
    fn shutdown_request_ends_the_accept_loop() {
        let name = format!(
            "terminalai-shutdown-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let server = DaemonServer::bind_named(&name).expect("bind shutdown socket");
        let server_thread = thread::spawn(move || server.serve());
        let client = DaemonClient::connect_named(&name).expect("connect shutdown socket");
        client.shutdown().expect("request daemon shutdown");
        drop(client);
        server_thread
            .join()
            .expect("server thread")
            .expect("serve after shutdown");
    }

    #[test]
    fn shutdown_request_persists_the_final_registry_state_before_server_exits() {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-final-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("final store directory");
        let path = dir.join("sessions.json");
        let cwd = std::env::current_dir().expect("cwd");
        let spec = LaunchSpec {
            agent: Agent::Claude,
            cwd: cwd.clone(),
            ..LaunchSpec::default()
        };
        let id = SessionId::new(1);
        let registry = SessionRegistry::from_store(SessionStoreSnapshot {
            spend: Vec::new(),
            magic: SESSION_STORE_MAGIC.to_owned(),
            schema_version: SESSION_STORE_SCHEMA_VERSION,
            sessions: vec![StoredSession {
                session: Session::new(id.clone(), &spec),
                spec: spec.clone(),
                command: ResolvedCommand {
                    program: "claude.exe".into(),
                    args: Vec::new(),
                    cwd,
                },
                scrollback: Vec::new(),
                queue: Default::default(),
            }],
            archives: Vec::new(),
            extra: Default::default(),
        });
        let writer = StoreWriter::spawn(path.clone(), registry.clone());
        let name = format!(
            "terminalai-final-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let server = DaemonServer::bind_named_with_state(
            &name,
            registry,
            Some(writer),
            None,
            None,
        )
        .expect("bind final store socket");
        let server_thread = thread::spawn(move || server.serve());
        let client = DaemonClient::connect_named(&name).expect("connect final store socket");
        client.shutdown().expect("request daemon shutdown");
        drop(client);
        server_thread
            .join()
            .expect("server thread")
            .expect("serve final store");

        let written = SessionStoreSnapshot::read(&path)
            .expect("read final session store")
            .expect("final session store exists");
        assert_eq!(written.sessions.len(), 1);
        assert_eq!(written.sessions[0].session.id, id);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_second_server_cannot_replace_the_first_binding() {
        let name = format!(
            "terminalai-single-instance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let first = DaemonServer::bind_named(&name).expect("first server binds");
        let second = DaemonServer::bind_named(&name);
        assert!(second.is_err(), "second daemon replaced the first binding");
        drop(first);
    }

    #[test]
    fn protocol_skew_names_the_running_daemon_and_stop_action() {
        let message = IpcError::VersionMismatch {
            daemon: 2,
            client: 3,
            daemon_pid: 4242,
        }
        .to_string();
        assert!(message.contains("PID 4242"));
        assert!(message.contains("v2"));
        assert!(message.contains("Stop-Process -Id 4242"));
    }

    #[cfg(windows)]
    #[test]
    fn pipe_acl_names_the_current_user_sid() {
        let sddl = current_user_pipe_sddl().expect("current user pipe SDDL");
        assert!(sddl.contains("(A;;GA;;;SY)"));
        assert!(
            sddl.contains("(A;;GA;;;S-"),
            "missing explicit user SID: {sddl}"
        );
        assert!(
            !sddl.contains("OW"),
            "owner-rights alias broadens under elevation: {sddl}"
        );
    }

    #[cfg(windows)]
    fn process_thread_count() -> usize {
        let script = format!(
            "(Get-Process -Id {} -ErrorAction Stop).Threads.Count",
            std::process::id()
        );
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .output()
            .expect("count process threads");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("thread count")
    }

    #[cfg(windows)]
    #[test]
    fn subscribed_connections_do_not_leave_event_or_writer_threads() {
        let baseline = process_thread_count();
        for index in 0..100 {
            let name = format!(
                "terminalai-r47-{}-{}-{}",
                std::process::id(),
                index,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            );
            let server = DaemonServer::bind_named(&name).expect("bind test socket");
            let server_thread = thread::spawn(move || server.serve_one());
            let client = DaemonClient::connect_named(&name).expect("connect test socket");
            client.subscribe().expect("subscribe test socket");
            assert!(matches!(client.call(Request::Close), Ok(Response::Ok)));
            drop(client);
            server_thread
                .join()
                .expect("server thread")
                .expect("serve client");
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let after = loop {
            let count = process_thread_count();
            if count <= baseline + 2 || std::time::Instant::now() >= deadline {
                break count;
            }
            thread::sleep(Duration::from_millis(25));
        };
        assert!(
            after <= baseline + 2,
            "connection threads leaked: baseline={baseline}, after={after}"
        );
    }
}
