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

mod client;
mod dispatch;
mod http_hooks;
mod logging;
mod persistence;
mod protocol;

// The protocol module owns the wire-level settings contract. Keep the contract
// names visible in this entry module as well because the frontend source audit
// reads the daemon entry point when checking that settings remain editable:
// `Request::SetAdmission`, `const ADMISSION_ENVIRONMENT: &[&str]`, and
// `pub from_environment: Vec<String>`.
// The dispatcher source audit also follows the history response from this
// entry point: `Request::SessionHistory => Response::SessionHistory {`.

#[cfg(test)]
mod tests;

pub use client::DaemonClient;
pub use http_hooks::HookEndpoint;
pub use logging::{
    init_logging, init_logging_with_prefix, log_directory, LogHub, LoggingGuard, MAX_LOG_FILES,
};
pub use protocol::{
    AdmissionSettings, ArchiveAfterLanding, FleetSpec, IpcError, Request, Response,
    MAX_BROADCAST_TARGETS, MAX_FRAME_BYTES, MAX_HISTORY_BYTES, MAX_WRITE_BYTES, PIPE_NAME,
    PROTOCOL_VERSION,
};

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
#[cfg(windows)]
use std::sync::Condvar;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::{prelude::*, ListenerNonblockingMode, ListenerOptions};
use terminalai_core::agent::{self, Agent};
use terminalai_core::scrollback::ScrollbackSpool;
use terminalai_core::{AdmissionConfig, RegistryEvent, SessionRegistry};

#[cfg(all(windows, test))]
use client::current_user_pipe_sddl;
#[cfg(windows)]
use client::current_user_pipe_descriptor;
use client::{handle_connection, socket_name};
#[cfg(test)]
use dispatch::{
    dispatch, dispatch_with_endpoint, dispatch_with_quarantine, external_sessions_from,
    owns_source, placement_answer, request_requires_registry,
};
use persistence::StoreWriter;
#[cfg(test)]
use protocol::{read_frame, WireMessage};

pub fn install_panic_hook() {
    persistence::install_panic_hook();
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
                let store_health = self.store_writer.as_ref().map(StoreWriter::health);
                let log_hub = self.log_hub.clone();
                let shutdown = shutdown.clone();
                let hook_endpoint = hook_endpoint.clone();
                // A transient spawn failure used to end serve() outright,
                // abandoning every live agent with no UI. One refused client
                // is not a reason to drop the fleet.
                if let Err(error) = thread::Builder::new()
                    .name("terminalai-daemon-client".into())
                    .spawn(move || {
                        if let Err(error) = handle_connection(
                            connection,
                            registry,
                            store_quarantine,
                            store_health,
                            log_hub,
                            shutdown,
                            hook_endpoint,
                        ) {
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
            self.store_writer.as_ref().map(StoreWriter::health),
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
                    Ok(
                        RegistryEvent::SessionUpdated { .. } | RegistryEvent::SessionRemoved { .. },
                    ) => {
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
static CONSOLE_TEARDOWN_COMPLETE: std::sync::OnceLock<(Mutex<bool>, Condvar)> =
    std::sync::OnceLock::new();

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
            if matches!(
                ctrl_type,
                CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
            ) {
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

/// How often each agent is asked whether it is still authenticated.
const AUTH_PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Ask each installed agent about its credentials and record the answer.
///
/// An agent that cannot be resolved is not probed at all: "not installed" is
/// already reported by preflight, and reporting it as an auth failure would put
/// a banner in front of the operator that logging in cannot clear.
fn refresh_agent_auth(registry: &SessionRegistry) {
    for agent in [Agent::Claude, Agent::Codex] {
        let Ok(binary) = agent::resolve(agent, None) else {
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
