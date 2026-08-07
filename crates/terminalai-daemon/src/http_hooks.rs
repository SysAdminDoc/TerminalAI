//! Authenticated loopback ingestion for Claude Code HTTP hooks.
//!
//! The listener binds only to IPv4 loopback, uses an ephemeral port and keeps
//! a fresh bearer token for the daemon lifetime. A second, per-session token is
//! required before a hook can mutate a row; HTTP callers supply it in
//! `X-TerminalAI-Session-Token`, while the managed command adapter carries it
//! through the local control pipe. It deliberately implements a small, bounded
//! HTTP/1.1 reader instead of pulling a web framework into the daemon's release
//! path: hooks are single POSTs with JSON bodies, and every other request shape
//! is rejected before it can reach the session registry.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use terminalai_core::agent::Agent;
use terminalai_core::{parse_hook_in, HookSignal, SessionRegistry};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const ACCEPT_POLL: Duration = Duration::from_millis(25);
const HOOK_WORKER_COUNT: usize = 4;
const CONNECTION_QUEUE_CAPACITY: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEndpoint {
    /// Base URL; the agent-specific path is `/hooks/claude` or
    /// `/hooks/codex`.
    pub base_url: String,
    /// Exact `Host` value accepted by the listener.
    pub host: String,
    /// A daemon-lifetime transport secret. It is returned only over the named
    /// control pipe, whose DACL is the local authorization boundary; it is not
    /// sufficient to identify a supervised session.
    pub bearer_token: String,
}

impl HookEndpoint {
    pub fn url_for(&self, agent: Agent) -> String {
        format!("{}/hooks/{}", self.base_url, agent.command_name())
    }
}

pub struct HookIngress {
    endpoint: HookEndpoint,
    #[cfg(test)]
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl HookIngress {
    pub fn bind(registry: SessionRegistry) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let token = fresh_token()?;
        let host = address.to_string();
        let endpoint = HookEndpoint {
            base_url: format!("http://{host}"),
            host,
            bearer_token: token.clone(),
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let worker = thread::Builder::new()
            .name("terminalai-http-hooks".into())
            .spawn(move || serve(listener, registry, token, address, worker_shutdown))?;
        Ok(Self {
            endpoint,
            #[cfg(test)]
            address,
            shutdown,
            worker: Some(worker),
        })
    }

    pub fn endpoint(&self) -> HookEndpoint {
        self.endpoint.clone()
    }

    #[cfg(test)]
    fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for HookIngress {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return;
        };
        if worker.thread().id() != thread::current().id() {
            let _ = worker.join();
        }
    }
}

fn serve(
    listener: TcpListener,
    registry: SessionRegistry,
    token: String,
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
) {
    let (connections, receiver) = sync_channel(CONNECTION_QUEUE_CAPACITY);
    let receiver = Arc::new(Mutex::new(receiver));
    let mut workers = Vec::with_capacity(HOOK_WORKER_COUNT);
    for index in 0..HOOK_WORKER_COUNT {
        let receiver = Arc::clone(&receiver);
        let registry = registry.clone();
        let token = token.clone();
        let shutdown = shutdown.clone();
        match thread::Builder::new()
            .name(format!("terminalai-http-hook-{index}"))
            .spawn(move || worker_loop(receiver, registry, token, address, shutdown))
        {
            Ok(worker) => workers.push(worker),
            Err(error) => tracing::warn!(%error, index, "could not start HTTP hook worker"),
        }
    }

    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if connections
                    .try_send(AcceptedConnection { stream, peer })
                    .is_err()
                {
                    tracing::debug!(%peer, "HTTP hook connection dropped because the worker queue is full");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(error) => {
                tracing::warn!(error = %error, "HTTP hook listener failed to accept");
                thread::sleep(ACCEPT_POLL);
            }
        }
    }

    drop(connections);
    for worker in workers {
        let _ = worker.join();
    }
}

fn worker_loop(
    receiver: Arc<Mutex<Receiver<AcceptedConnection>>>,
    registry: SessionRegistry,
    token: String,
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        let connection = match receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recv_timeout(ACCEPT_POLL)
        {
            Ok(connection) => connection,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let peer = connection.peer;
        let result = catch_unwind(AssertUnwindSafe(|| {
            handle_connection(connection.stream, &registry, &token, address)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(%peer, error = %error, "HTTP hook request rejected");
            }
            Err(_) => {
                tracing::error!(%peer, "HTTP hook worker recovered from a panic");
            }
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    registry: &SessionRegistry,
    token: &str,
    address: SocketAddr,
) -> io::Result<()> {
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let request = match read_request(&mut stream, deadline)? {
        Ok(request) => request,
        Err(rejection) => {
            write_response(&mut stream, rejection.status, &rejection.message)?;
            return Ok(());
        }
    };
    let expected_host = address.to_string();
    if request.method != "POST" {
        return write_response(&mut stream, 405, "method not allowed");
    }
    if request.headers.contains_key("origin") {
        return write_response(&mut stream, 403, "origin header is not accepted");
    }
    if request.headers.get("host").map(String::as_str) != Some(expected_host.as_str()) {
        return write_response(&mut stream, 403, "host is not allowlisted");
    }
    let expected_authorization = format!("Bearer {token}");
    if !constant_time_equal(
        request
            .headers
            .get("authorization")
            .map(String::as_str)
            .unwrap_or_default(),
        &expected_authorization,
    ) {
        return write_response(&mut stream, 401, "invalid bearer token");
    }
    let agent = match request.path.as_str() {
        "/hooks/claude" => Agent::Claude,
        "/hooks/codex" => Agent::Codex,
        _ => return write_response(&mut stream, 404, "unknown hook endpoint"),
    };
    let payload = match std::str::from_utf8(&request.body) {
        Ok(payload) => payload,
        Err(_) => return write_response(&mut stream, 400, "hook body is not UTF-8"),
    };
    let event = match parse_hook_in(agent, payload, None) {
        Ok(event) => event,
        Err(error) => return write_response(&mut stream, 400, &error.to_string()),
    };
    if let HookSignal::Unknown { event: hook_name } = &event.signal {
        tracing::warn!(
            agent = ?event.agent,
            session_id = ?event.session_id,
            hook_event = %hook_name,
            "unknown HTTP hook event observed"
        );
    }
    if Instant::now() >= deadline {
        return write_response(&mut stream, 408, "hook request deadline exceeded");
    }
    let session_token = request
        .headers
        .get("x-terminalai-session-token")
        .map(String::as_str);
    let _matched = registry.apply_hook_with_token(event, session_token);
    write_response(&mut stream, 202, "accepted")
}

struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[cfg_attr(test, derive(Debug))]
struct Rejection {
    status: u16,
    message: String,
}

struct AcceptedConnection {
    stream: TcpStream,
    peer: SocketAddr,
}

fn read_request(
    stream: &mut TcpStream,
    deadline: Instant,
) -> io::Result<Result<HttpRequest, Rejection>> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP hook request deadline exceeded",
            ));
        }
        stream.set_read_timeout(Some(remaining.min(READ_TIMEOUT)))?;
        if bytes.len() > MAX_HEADER_BYTES {
            return Ok(Err(Rejection {
                status: 431,
                message: "request headers are too large".into(),
            }));
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(Err(Rejection {
                status: 400,
                message: "request ended before headers".into(),
            }));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request headers are not UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(Err(Rejection {
            status: 400,
            message: "request line is missing".into(),
        }));
    };
    let parts: Vec<_> = request_line.split_ascii_whitespace().collect();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" {
        return Ok(Err(Rejection {
            status: 400,
            message: "HTTP/1.1 request required".into(),
        }));
    }
    let mut headers = HashMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Ok(Err(Rejection {
                status: 400,
                message: "malformed request header".into(),
            }));
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if name.is_empty() || headers.insert(name, value).is_some() {
            return Ok(Err(Rejection {
                status: 400,
                message: "duplicate or empty request header".into(),
            }));
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Ok(Err(Rejection {
            status: 400,
            message: "chunked requests are not accepted".into(),
        }));
    }
    let Some(content_length) = headers.get("content-length") else {
        return Ok(Err(Rejection {
            status: 411,
            message: "content length is required".into(),
        }));
    };
    let Ok(content_length) = content_length.parse::<usize>() else {
        return Ok(Err(Rejection {
            status: 400,
            message: "content length is invalid".into(),
        }));
    };
    if content_length > MAX_BODY_BYTES {
        return Ok(Err(Rejection {
            status: 413,
            message: "hook body is too large".into(),
        }));
    }
    let available = bytes.len().saturating_sub(header_end);
    if available > content_length {
        return Ok(Err(Rejection {
            status: 400,
            message: "request contains bytes beyond the body".into(),
        }));
    }
    let mut body = bytes[header_end..].to_vec();
    body.resize(content_length, 0);
    if let Some(rejection) = read_body(stream, &mut body, available, deadline)? {
        return Ok(Err(rejection));
    }
    Ok(Ok(HttpRequest {
        method: parts[0].to_owned(),
        path: parts[1].to_owned(),
        headers,
        body,
    }))
}

/// A source that can be read with a per-read timeout.
///
/// Exists so the deadline behaviour below can be tested against a fake that
/// never delivers, rather than against a real socket and a real five seconds —
/// a wall-clock test of a timeout is the kind that passes on an idle machine and
/// fails during a release build.
trait DeadlineRead {
    fn arm(&mut self, timeout: Duration) -> io::Result<()>;
    fn read_some(&mut self, buffer: &mut [u8]) -> io::Result<usize>;
}

impl DeadlineRead for TcpStream {
    fn arm(&mut self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }

    fn read_some(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.read(buffer)
    }
}

/// Fill `body` from `filled` onwards, giving up at `deadline`.
///
/// The deadline is re-checked before every read, exactly as the header loop
/// does. `read_exact` cannot do this: it arms the timeout once and then loops
/// internally, so each individual read gets the full `READ_TIMEOUT` and every
/// successful byte re-arms it. A client that declares a megabyte and trickles
/// one byte just inside the timeout holds a worker indefinitely — and none of
/// this has reached the bearer check yet, which runs on the fully-read request.
/// Four workers behind a sixteen-deep queue means four such connections stop
/// hook ingestion for the whole fleet, and status updates simply stop arriving.
fn read_body(
    stream: &mut impl DeadlineRead,
    body: &mut [u8],
    filled: usize,
    deadline: Instant,
) -> io::Result<Option<Rejection>> {
    let mut filled = filled;
    while filled < body.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP hook request deadline exceeded",
            ));
        }
        stream.arm(remaining.min(READ_TIMEOUT))?;
        let read = stream.read_some(&mut body[filled..])?;
        if read == 0 {
            return Ok(Some(Rejection {
                status: 400,
                message: "request ended before the declared body".into(),
            }));
        }
        filled += read;
    }
    Ok(None)
}

fn write_response(stream: &mut TcpStream, status: u16, message: &str) -> io::Result<()> {
    let body = format!(
        r#"{{"ok":{},"message":{}}}"#,
        status < 300,
        serde_json::to_string(message).unwrap_or_else(|_| "\"request\"".into())
    );
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason_phrase(status),
        body.len()
    )?;
    stream.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        408 => "Request Timeout",
        431 => "Request Header Fields Too Large",
        _ => "Bad Request",
    }
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    let mut difference = left.len() ^ right.len();
    for (a, b) in left.bytes().zip(right.bytes()) {
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

fn fresh_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| io::Error::other(format!("could not generate hook token: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Instant;

    fn request(_endpoint: &HookEndpoint, host: &str, authorization: &str, extra: &str) -> String {
        let body = r#"{"hook_event_name":"SessionStart","session_id":"missing"}"#;
        format!(
            "POST /hooks/claude HTTP/1.1\r\nHost: {host}\r\nAuthorization: {authorization}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    fn response(address: SocketAddr, request: String) -> String {
        response_with_timeout(address, request, Duration::from_secs(2)).expect("HTTP response")
    }

    fn response_with_timeout(
        address: SocketAddr,
        request: String,
        timeout: Duration,
    ) -> io::Result<String> {
        let mut stream = TcpStream::connect(address).expect("connect HTTP hook listener");
        stream.set_read_timeout(Some(timeout))?;
        stream
            .write_all(request.as_bytes())
            .map_err(|error| io::Error::other(format!("write HTTP request: {error}")))?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response)
    }

    #[test]
    fn endpoint_is_loopback_and_token_is_fresh() {
        let first = HookIngress::bind(SessionRegistry::new()).expect("first listener");
        let second = HookIngress::bind(SessionRegistry::new()).expect("second listener");
        assert!(first.endpoint().base_url.starts_with("http://127.0.0.1:"));
        assert_eq!(first.endpoint().host, first.address().to_string());
        assert_ne!(
            first.endpoint().bearer_token,
            second.endpoint().bearer_token
        );
    }

    #[test]
    fn authenticated_http_hook_is_accepted_and_origin_is_rejected() {
        let ingress = HookIngress::bind(SessionRegistry::new()).expect("listener");
        let endpoint = ingress.endpoint();
        let authorization = format!("Bearer {}", endpoint.bearer_token);
        let accepted = response(
            ingress.address(),
            request(&endpoint, &endpoint.host, &authorization, ""),
        );
        assert!(accepted.starts_with("HTTP/1.1 202 Accepted"), "{accepted}");

        let rejected = response(
            ingress.address(),
            request(
                &endpoint,
                &endpoint.host,
                &authorization,
                "Origin: http://untrusted.example\r\n",
            ),
        );
        assert!(rejected.starts_with("HTTP/1.1 403 Forbidden"), "{rejected}");
    }

    #[test]
    fn an_http_hook_without_a_cwd_does_not_adopt_the_daemon_s_directory() {
        let event = parse_hook_in(Agent::Claude, r#"{"hook_event_name":"SessionStart"}"#, None)
            .expect("hook payload");
        assert_eq!(event.cwd, None);
    }

    #[test]
    fn wrong_host_and_token_are_rejected() {
        let ingress = HookIngress::bind(SessionRegistry::new()).expect("listener");
        let endpoint = ingress.endpoint();
        let wrong_host = response(
            ingress.address(),
            request(
                &endpoint,
                "127.0.0.1:1",
                &format!("Bearer {}", endpoint.bearer_token),
                "",
            ),
        );
        assert!(
            wrong_host.starts_with("HTTP/1.1 403 Forbidden"),
            "{wrong_host}"
        );
        let wrong_token = response(
            ingress.address(),
            request(&endpoint, &endpoint.host, "Bearer wrong", ""),
        );
        assert!(
            wrong_token.starts_with("HTTP/1.1 401 Unauthorized"),
            "{wrong_token}"
        );
    }

    /// A source that always has one more byte and never finishes — the shape of
    /// a client trickling a declared body just inside the per-read timeout.
    struct Trickle {
        reads: usize,
    }

    impl DeadlineRead for Trickle {
        fn arm(&mut self, _timeout: Duration) -> io::Result<()> {
            Ok(())
        }

        fn read_some(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            buffer[0] = b'x';
            Ok(1)
        }
    }

    #[test]
    fn a_body_that_never_arrives_stops_at_the_deadline() {
        // `read_exact` arms the timeout once and loops internally, so every
        // successful byte re-arms it and a trickling client holds a worker
        // indefinitely — before authentication, since the bearer check runs on
        // the fully-read request. Four such connections stop hook ingestion for
        // the whole fleet.
        let mut stream = Trickle { reads: 0 };
        let mut body = vec![0u8; 64];
        let error = read_body(&mut stream, &mut body, 0, Instant::now())
            .expect_err("a deadline already spent must not read at all");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(stream.reads, 0, "the deadline is checked before each read");
    }

    #[test]
    fn a_body_that_arrives_in_pieces_is_still_assembled() {
        // The bound must not cost correctness: a body split across reads, which
        // is the ordinary case on a loopback socket, still completes.
        let mut stream = Trickle { reads: 0 };
        let mut body = vec![0u8; 4];
        let rejection = read_body(
            &mut stream,
            &mut body,
            0,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("a live deadline reads the body");
        assert!(rejection.is_none());
        assert_eq!(&body, b"xxxx");
        assert_eq!(stream.reads, 4, "one byte per read, four reads");
    }

    #[test]
    fn a_stalled_connection_does_not_block_a_second_hook() {
        let ingress = HookIngress::bind(SessionRegistry::new()).expect("listener");
        let endpoint = ingress.endpoint();
        let stalled = TcpStream::connect(ingress.address()).expect("connect stalled client");
        thread::sleep(Duration::from_millis(50));

        let authorization = format!("Bearer {}", endpoint.bearer_token);
        let started = Instant::now();
        let accepted = response_with_timeout(
            ingress.address(),
            request(&endpoint, &endpoint.host, &authorization, ""),
            Duration::from_secs(1),
        )
        .expect("second hook response");
        assert!(accepted.starts_with("HTTP/1.1 202 Accepted"), "{accepted}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "second hook waited behind the stalled connection: {accepted}"
        );
        drop(stalled);
    }
}
