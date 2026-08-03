//! Authenticated loopback ingestion for Claude Code HTTP hooks.
//!
//! The listener binds only to IPv4 loopback, uses an ephemeral port and keeps
//! a fresh bearer token for the daemon lifetime. It deliberately implements a
//! small, bounded HTTP/1.1 reader instead of pulling a web framework into the
//! daemon's release path: hooks are single POSTs with JSON bodies, and every
//! other request shape is rejected before it can reach the session registry.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use terminalai_core::agent::Agent;
use terminalai_core::{parse_hook, HookSignal, SessionRegistry};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEndpoint {
    /// Base URL; the agent-specific path is `/hooks/claude` or
    /// `/hooks/codex`.
    pub base_url: String,
    /// Exact `Host` value accepted by the listener.
    pub host: String,
    /// A daemon-lifetime bearer secret. It is returned only over the named
    /// control pipe, whose DACL is the local authorization boundary.
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
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if let Err(error) = handle_connection(stream, &registry, &token, address) {
                    tracing::debug!(%peer, error = %error, "HTTP hook request rejected");
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
}

fn handle_connection(
    mut stream: TcpStream,
    registry: &SessionRegistry,
    token: &str,
    address: SocketAddr,
) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let request = match read_request(&mut stream)? {
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
    let event = match parse_hook(agent, payload) {
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
    let _matched = registry.apply_hook(event);
    write_response(&mut stream, 202, "accepted")
}

struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct Rejection {
    status: u16,
    message: String,
}

fn read_request(stream: &mut TcpStream) -> io::Result<Result<HttpRequest, Rejection>> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
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
    if body.len() > available {
        stream.read_exact(&mut body[available..])?;
    }
    Ok(Ok(HttpRequest {
        method: parts[0].to_owned(),
        path: parts[1].to_owned(),
        headers,
        body,
    }))
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

    fn request(_endpoint: &HookEndpoint, host: &str, authorization: &str, extra: &str) -> String {
        let body = r#"{"hook_event_name":"SessionStart","session_id":"missing"}"#;
        format!(
            "POST /hooks/claude HTTP/1.1\r\nHost: {host}\r\nAuthorization: {authorization}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    fn response(address: SocketAddr, request: String) -> String {
        let mut stream = TcpStream::connect(address).expect("connect HTTP hook listener");
        stream
            .write_all(request.as_bytes())
            .expect("write HTTP request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
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
}
