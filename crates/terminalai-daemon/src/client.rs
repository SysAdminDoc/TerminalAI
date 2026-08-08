use std::collections::HashMap;
use std::io::{self, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::{prelude::*, GenericNamespaced, Name, RecvHalf, SendHalf};
use terminalai_core::{LogEntry, RegistryEvent, SessionRegistry};

use super::dispatch::dispatch_with_endpoint;
use super::http_hooks::HookEndpoint;
use super::persistence;
use super::protocol::{
    read_frame, IpcError, Request, Response, WireMessage, EVENT_POLL_INTERVAL, LEGACY_PIPE_NAME,
    OUTGOING_QUEUE_CAPACITY, PIPE_NAME, PROTOCOL_VERSION,
};

pub(super) fn handle_connection(
    stream: LocalSocketStream,
    registry: SessionRegistry,
    store_quarantine: Option<String>,
    store_health: Option<persistence::StoreHealth>,
    log_hub: Option<super::LogHub>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    hook_endpoint: HookEndpoint,
) -> Result<(), IpcError> {
    let peer_pid = normalize_peer_pid(stream.peer_creds()?.pid());
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
                // connection may still carry sane traffic afterwards only if
                // the stream is resynchronized - which it cannot be mid-frame.
                // Say why and close.
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
                    shutdown.store(true, std::sync::atomic::Ordering::Release);
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
                            log_hub.as_ref().map(super::LogHub::subscribe),
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
                        store_health.as_ref(),
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

/// Normalize the peer's process id to the type the handshake compares against.
///
/// `interprocess` reports it as the platform's own: `pid_t` — a *signed* `i32` —
/// on Unix, and `u32` on Windows. The client declares a `u32`, so the conversion
/// belongs at this one boundary rather than at each use. A negative `pid_t`
/// names a process group, never a peer, so it becomes `None` rather than
/// wrapping into a large positive id that could collide with a real one.
#[cfg(unix)]
fn normalize_peer_pid(pid: Option<i32>) -> Option<u32> {
    pid.and_then(|pid| u32::try_from(pid).ok())
}

#[cfg(windows)]
fn normalize_peer_pid(pid: Option<u32>) -> Option<u32> {
    pid
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

pub(super) fn socket_name(name: &str) -> Result<Name<'static>, IpcError> {
    name.to_ns_name::<GenericNamespaced>()
        .map(Name::into_owned)
        .map_err(IpcError::Io)
}

#[cfg(windows)]
pub(super) fn current_user_pipe_descriptor(
) -> Result<interprocess::os::windows::security_descriptor::SecurityDescriptor, IpcError> {
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;

    let sddl = U16CString::from_str(&current_user_pipe_sddl()?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    SecurityDescriptor::deserialize(sddl.as_ucstr()).map_err(IpcError::Io)
}

#[cfg(windows)]
pub(super) fn current_user_pipe_sddl() -> Result<String, IpcError> {
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
