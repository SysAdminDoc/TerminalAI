//! The TerminalAI control plane.
//!
//! The daemon is the only process that owns [`terminalai_core::PtySession`]
//! values. The GUI and CLI are clients of a versioned, line-framed protocol
//! over a local socket (a Windows named pipe on the primary platform). Events
//! are sent only after an explicit `Subscribe` request, so broadcasts cannot be
//! mistaken for RPC responses.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::{
    prelude::*, GenericNamespaced, ListenerOptions, Name, RecvHalf, SendHalf,
};
use serde::{Deserialize, Serialize};
use terminalai_core::agent::{self, Agent, Origin};
use terminalai_core::launch::LaunchSpec;
use terminalai_core::pty::PtySize;
use terminalai_core::{HookEvent, RegistryEvent, Session, SessionId, SessionRegistry};

pub const PROTOCOL_VERSION: u16 = 1;
pub const PIPE_NAME: &str = "terminalai.control.v1";

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
    #[error("incompatible control protocol: daemon={daemon}, client={client}")]
    VersionMismatch { daemon: u16, client: u16 },
    #[error("control peer identity mismatch: expected process {expected}, got {actual:?}")]
    PeerMismatch { expected: u32, actual: Option<u32> },
    #[error("invalid control message: {0}")]
    InvalidMessage(String),
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
    Snapshot,
    Resolve {
        agent: Agent,
        configured_path: Option<PathBuf>,
    },
    Preview {
        spec: Box<LaunchSpec>,
        configured_path: Option<PathBuf>,
    },
    Hook {
        event: HookEvent,
    },
    Launch {
        spec: Box<LaunchSpec>,
        configured_path: Option<PathBuf>,
    },
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
    Scrollback {
        id: SessionId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u16,
        daemon_pid: u32,
    },
    Ok,
    Pong,
    Snapshot {
        sessions: Vec<Session>,
        focused: Option<SessionId>,
    },
    Resolved {
        agent: Agent,
        path: PathBuf,
        origin: String,
    },
    Preview {
        command: String,
    },
    Hook {
        matched: bool,
    },
    Launched {
        id: SessionId,
    },
    Scrollback {
        data: String,
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
    Response { id: u64, response: Response },
    Event { event: RegistryEvent },
}

pub struct DaemonServer {
    listener: LocalSocketListener,
    registry: SessionRegistry,
}

impl DaemonServer {
    pub fn bind() -> Result<Self, IpcError> {
        Self::bind_named(PIPE_NAME)
    }

    pub fn bind_named(name: &str) -> Result<Self, IpcError> {
        let name = socket_name(name)?;
        let mut options = ListenerOptions::new().name(name);
        #[cfg(windows)]
        {
            use interprocess::os::windows::local_socket::ListenerOptionsExt;
            options = options.security_descriptor(current_user_pipe_descriptor()?);
        }
        // Interprocess sets FILE_FLAG_FIRST_PIPE_INSTANCE for the initial
        // Windows named-pipe instance. It also rejects remote clients by
        // default; the DACL below narrows local access to SYSTEM and the owner.
        let listener = options.create_sync()?;
        Ok(Self {
            listener,
            registry: SessionRegistry::new(),
        })
    }

    pub fn with_registry(name: &str, registry: SessionRegistry) -> Result<Self, IpcError> {
        let mut server = Self::bind_named(name)?;
        server.registry = registry;
        Ok(server)
    }

    /// Serve connections until the daemon process is terminated.
    pub fn serve(self) -> Result<(), IpcError> {
        for connection in self.listener.incoming() {
            match connection {
                Ok(stream) => {
                    let registry = self.registry.clone();
                    thread::Builder::new()
                        .name("terminalai-daemon-client".into())
                        .spawn(move || {
                            if let Err(error) = handle_connection(stream, registry) {
                                eprintln!("terminalai-daemon client: {error}");
                            }
                        })
                        .map_err(IpcError::Io)?;
                }
                Err(error) => eprintln!("terminalai-daemon accept: {error}"),
            }
        }
        Ok(())
    }

    /// Test and embedding hook: handle one client and return when it closes.
    pub fn serve_one(&self) -> Result<(), IpcError> {
        let stream = self
            .listener
            .incoming()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "listener closed"))??;
        handle_connection(stream, self.registry.clone())
    }
}

pub fn run() -> Result<(), IpcError> {
    DaemonServer::bind()?.serve()
}

fn handle_connection(stream: LocalSocketStream, registry: SessionRegistry) -> Result<(), IpcError> {
    let peer_pid = stream.peer_creds()?.pid();
    let (receive, send) = stream.split();
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<WireMessage>();
    let writer = thread::Builder::new()
        .name("terminalai-daemon-writer".into())
        .spawn(move || write_messages(send, outgoing_rx))
        .map_err(IpcError::Io)?;

    let mut reader = BufReader::new(receive);
    let mut authenticated = false;
    let mut subscribed = false;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let message: WireMessage = serde_json::from_str(&line)
            .map_err(|error| IpcError::InvalidMessage(error.to_string()))?;
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
                send_response(
                    &outgoing_tx,
                    id,
                    Response::Error {
                        message: IpcError::VersionMismatch {
                            daemon: PROTOCOL_VERSION,
                            client: protocol,
                        }
                        .to_string(),
                    },
                )?;
                break;
            }
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
            Request::Subscribe => {
                if !subscribed {
                    subscribed = true;
                    bridge_events(registry.subscribe(), outgoing_tx.clone());
                }
                send_response(&outgoing_tx, id, Response::Ok)?;
            }
            request => {
                let response = dispatch(request, &registry);
                send_response(&outgoing_tx, id, response)?;
            }
        }
    }
    drop(outgoing_tx);
    writer
        .join()
        .map_err(|_| IpcError::Io(io::Error::other("writer thread panicked")))??;
    Ok(())
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
    outgoing: &Sender<WireMessage>,
    id: u64,
    response: Response,
) -> Result<(), IpcError> {
    outgoing
        .send(WireMessage::Response { id, response })
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client disconnected").into())
}

fn bridge_events(events: Receiver<RegistryEvent>, outgoing: Sender<WireMessage>) {
    let _ = thread::Builder::new()
        .name("terminalai-daemon-events".into())
        .spawn(move || {
            for event in events {
                if outgoing.send(WireMessage::Event { event }).is_err() {
                    break;
                }
            }
        });
}

fn dispatch(request: Request, registry: &SessionRegistry) -> Response {
    match request {
        Request::Hello { .. } => Response::Error {
            message: "Hello was already completed".into(),
        },
        Request::Subscribe => Response::Error {
            message: "Subscribe is handled by the connection".into(),
        },
        Request::Close => Response::Ok,
        Request::Ping => Response::Pong,
        Request::Snapshot => Response::Snapshot {
            sessions: registry.snapshot(),
            focused: registry.focused(),
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
        Request::Preview {
            spec,
            configured_path,
        } => {
            let spec = *spec;
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
        Request::Hook { event } => Response::Hook {
            matched: registry.apply_hook(event),
        },
        Request::Launch {
            spec,
            configured_path,
        } => {
            let spec = *spec;
            match agent::resolve(spec.agent, configured_path.as_deref()) {
                Ok(binary) => match registry.launch(spec, binary) {
                    Ok(id) => Response::Launched { id },
                    Err(error) => Response::Error {
                        message: resolve_registry_error(error),
                    },
                },
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::Write { id, data } => match registry.write(&id, data.as_bytes()) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
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
        Request::Scrollback { id } => match registry.scrollback(&id) {
            Ok(data) => Response::Scrollback {
                data: String::from_utf8_lossy(&data).into_owned(),
            },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
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
        Self::connect_named(PIPE_NAME)
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
            Response::Hello { protocol, .. } => Err(IpcError::VersionMismatch {
                daemon: protocol,
                client: PROTOCOL_VERSION,
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
            loop {
                line.clear();
                let read = match reader.read_line(&mut line) {
                    Ok(read) => read,
                    Err(_) => break,
                };
                if read == 0 {
                    break;
                }
                let Ok(message) = serde_json::from_str::<WireMessage>(&line) else {
                    continue;
                };
                match message {
                    WireMessage::Response { id, response } => {
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

    // `OW` is the owner-rights SID, which resolves to the user that owns the
    // pipe object. SYSTEM is retained for service/diagnostic tooling. There is
    // no Everyone or remote-client ACE, and no impersonation is used.
    let sddl = U16CString::from_str("D:P(A;;GA;;;SY)(A;;GA;;;OW)")
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    SecurityDescriptor::deserialize(sddl.as_ucstr()).map_err(IpcError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(matches!(client.call(Request::Ping), Ok(Response::Pong)));
        assert!(matches!(client.call(Request::Close), Ok(Response::Ok)));
        drop(client);
        server_thread
            .join()
            .expect("server thread")
            .expect("serve client");
    }
}
