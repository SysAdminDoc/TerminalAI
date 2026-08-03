//! A read-mostly MCP server over the fleet.
//!
//! No tool in the survey unifies both vendors' session lists behind one
//! interface, so an agent cannot ask what its siblings are doing. This exposes
//! that, and deliberately almost nothing else.
//!
//! Three rules, in order of how much damage getting them wrong would do:
//!
//! 1. **Reads are ungated; writes are not.** Listing sessions and reading their
//!    status is harmless and useful. Spawning, killing or typing into a session
//!    is none of those, so each mutating call needs a token the operator passed
//!    out of band *and* a session that was opted in by id. A server that let any
//!    connected model type into any session would be a remote shell.
//! 2. **Transcript content is never exposed by default.** The last *line* of
//!    output is what a sibling needs to know a session is stuck; the transcript
//!    is the operator's conversation, and handing it to another model is a
//!    disclosure they did not ask for.
//! 3. **Tool metadata is static.** Tool poisoning — instructions smuggled into
//!    tool descriptions and re-executed on every invocation, the dominant 2026
//!    MCP attack class — is only possible if descriptions come from somewhere
//!    mutable. Every description here is a compile-time constant, and session
//!    data is only ever returned as tool *results*, never merged into metadata.
//!
//! The protocol layer is pure: it turns a request line into a response line and
//! calls a [`FleetAccess`] for anything it cannot answer itself, so the whole
//! surface is testable without a daemon.

use std::collections::BTreeSet;

use serde_json::{json, Value};

/// The MCP revision this server implements. Reported verbatim during
/// `initialize`; a client asking for a different one is told what we speak
/// rather than being guessed at.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "terminalai";

/// JSON-RPC error codes used here. The MCP-specific ones live above -32000.
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const PARSE_ERROR: i64 = -32700;

/// What the server needs from the fleet. Implemented against the daemon in
/// production and against a fake in tests.
pub trait FleetAccess {
    /// Every tracked session, as the daemon's `Session` JSON.
    fn sessions(&self) -> Result<Vec<Value>, String>;
    /// Sessions running outside this supervisor.
    fn external_sessions(&self) -> Result<Vec<Value>, String>;
    /// Fleet admission and cost totals.
    fn admission(&self) -> Result<Value, String>;
    /// The tail of one session's output, already bounded by the caller.
    fn last_output(&self, id: &str, max_lines: usize) -> Result<String, String>;
    /// Send text to a session. Only reached after the write gate allows it.
    fn write_session(&self, id: &str, data: &str) -> Result<(), String>;
    /// Stop a session. Only reached after the write gate allows it.
    fn kill_session(&self, id: &str) -> Result<(), String>;
    /// Record that a mutating tool ran, so every one is visible in the
    /// diagnostics timeline rather than only in this process's memory.
    fn log_mutation(&self, tool: &str, session: &str, allowed: bool, detail: &str);
}

/// Which mutating calls this server will perform, if any.
///
/// Default is the safe one: no token, no sessions, so every mutating tool
/// refuses. Both halves are required — a token alone does not make every
/// session writable, and opting a session in without a token does nothing.
#[derive(Debug, Clone, Default)]
pub struct WriteGate {
    token: Option<String>,
    sessions: BTreeSet<String>,
}

impl WriteGate {
    /// Allow mutating calls that present `token`, against `sessions` only.
    pub fn new(token: impl Into<String>, sessions: impl IntoIterator<Item = String>) -> Self {
        Self {
            token: Some(token.into()),
            sessions: sessions.into_iter().collect(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.token.is_some() && !self.sessions.is_empty()
    }

    /// Why this call is refused, or `None` when it may proceed.
    ///
    /// Returns one specific reason rather than a generic denial: an operator who
    /// opted a session in and still cannot write needs to know whether the token
    /// or the session id is the problem.
    fn refuse(&self, token: Option<&str>, session: &str) -> Option<String> {
        let Some(expected) = &self.token else {
            return Some(
                "this MCP server is read-only; start it with --write-token and --write-session to \
                 allow mutating tools"
                    .to_owned(),
            );
        };
        let Some(presented) = token else {
            return Some("this tool requires the write token in its `token` argument".to_owned());
        };
        if !constant_time_eq(presented, expected) {
            return Some("the write token is not valid".to_owned());
        }
        if !self.sessions.contains(session) {
            return Some(format!(
                "session {session} was not opted in; pass --write-session {session} when starting \
                 the server"
            ));
        }
        None
    }
}

/// Compare without leaking the matching prefix length through timing.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// One tool's static metadata.
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    mutating: bool,
    schema: fn() -> Value,
}

fn no_arguments() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn session_argument() -> Value {
    json!({
        "type": "object",
        "properties": { "session": { "type": "string", "description": "Session id, e.g. s0001" } },
        "required": ["session"],
        "additionalProperties": false
    })
}

fn last_output_arguments() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session": { "type": "string", "description": "Session id, e.g. s0001" },
            "lines": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_OUTPUT_LINES,
                "description": "How many trailing lines to return (default 20)"
            }
        },
        "required": ["session"],
        "additionalProperties": false
    })
}

fn write_arguments() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session": { "type": "string" },
            "text": { "type": "string", "description": "Sent as bracketed paste, then Enter" },
            "token": { "type": "string", "description": "The operator's out-of-band write token" }
        },
        "required": ["session", "text", "token"],
        "additionalProperties": false
    })
}

fn kill_arguments() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session": { "type": "string" },
            "token": { "type": "string", "description": "The operator's out-of-band write token" }
        },
        "required": ["session", "token"],
        "additionalProperties": false
    })
}

/// Upper bound on returned output lines, so one call cannot pull a whole
/// scrollback through a tool result.
pub const MAX_OUTPUT_LINES: usize = 200;
const DEFAULT_OUTPUT_LINES: usize = 20;

const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "list_sessions",
        description: "List every agent session this fleet supervises, with its status, agent, \
                      working directory, model and dwell time. Read-only.",
        mutating: false,
        schema: no_arguments,
    },
    ToolSpec {
        name: "list_external_sessions",
        description: "List Claude Code and Codex sessions running outside this supervisor, \
                      discovered from the agents' own registries. Read-only.",
        mutating: false,
        schema: no_arguments,
    },
    ToolSpec {
        name: "session_status",
        description: "Read one session's current status, phase, health and rate-limit state. \
                      Read-only.",
        mutating: false,
        schema: session_argument,
    },
    ToolSpec {
        name: "session_last_output",
        description: "Read the last few lines a session printed. This is terminal output only; \
                      the session's transcript is never exposed. Read-only.",
        mutating: false,
        schema: last_output_arguments,
    },
    ToolSpec {
        name: "fleet_cost",
        description: "Read the fleet's aggregate spend, how many sessions reported a cost, and \
                      which price table produced the figure. Read-only.",
        mutating: false,
        schema: no_arguments,
    },
    ToolSpec {
        name: "send_to_session",
        description: "Type text into a session. Requires the operator's write token and a session \
                      that was explicitly opted in when the server was started.",
        mutating: true,
        schema: write_arguments,
    },
    ToolSpec {
        name: "stop_session",
        description: "Stop a running session. Requires the operator's write token and a session \
                      that was explicitly opted in when the server was started.",
        mutating: true,
        schema: kill_arguments,
    },
];

pub struct McpServer<F: FleetAccess> {
    fleet: F,
    gate: WriteGate,
    initialized: bool,
}

impl<F: FleetAccess> McpServer<F> {
    pub fn new(fleet: F, gate: WriteGate) -> Self {
        Self {
            fleet,
            gate,
            initialized: false,
        }
    }

    /// Consume the server and return its fleet, so a test can inspect what the
    /// tools actually did rather than only what they reported.
    pub fn into_fleet(self) -> F {
        self.fleet
    }

    /// Handle one JSON-RPC line.
    ///
    /// `None` means the message was a notification and warrants no reply, which
    /// the transport must honour: answering a notification is a protocol error
    /// that some clients treat as fatal.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                return Some(error_response(
                    Value::Null,
                    PARSE_ERROR,
                    &format!("invalid JSON: {error}"),
                ))
            }
        };
        let id = value.get("id").cloned();
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            // A response or a malformed frame. Never answer a response.
            return id.map(|id| error_response(id, INVALID_REQUEST, "missing method"));
        };
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let result = self.dispatch(method, &params);

        match (id, result) {
            // Notification: no id, so no reply regardless of outcome.
            (None, _) => None,
            (Some(id), Ok(result)) => Some(success_response(id, result)),
            (Some(id), Err((code, message))) => Some(error_response(id, code, &message)),
        }
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                self.initialized = true;
                Ok(json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    // Stated in the handshake so a client sees the boundary
                    // before it calls anything.
                    "instructions": if self.gate.is_enabled() {
                        "Read tools are open. Mutating tools require the operator's write token \
                         and only work on sessions opted in at startup. Session transcripts are \
                         never exposed."
                    } else {
                        "This fleet is exposed read-only. Session transcripts are never exposed."
                    },
                }))
            }
            "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_descriptors(&self.gate) })),
            "tools/call" => self.call_tool(params),
            // Declared capabilities do not include these, so a client asking is
            // told plainly rather than getting an empty list it might cache.
            "resources/list" | "prompts/list" => Err((
                METHOD_NOT_FOUND,
                format!("{SERVER_NAME} exposes tools only"),
            )),
            other => Err((METHOD_NOT_FOUND, format!("unknown method {other}"))),
        }
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "tools/call needs a name".to_owned()))?;
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let spec = TOOLS
            .iter()
            .find(|tool| tool.name == name)
            .ok_or((INVALID_PARAMS, format!("unknown tool {name}")))?;

        // A mutating tool that is not enabled is hidden from tools/list, so
        // reaching one by name is either a stale client or a probe. Either way
        // it is refused and recorded.
        if spec.mutating && !self.gate.is_enabled() {
            let session = string_argument(&arguments, "session").unwrap_or_default();
            let detail = "server is read-only";
            self.fleet.log_mutation(name, &session, false, detail);
            return Ok(tool_error(detail));
        }

        match name {
            "list_sessions" => self.read(|fleet| {
                let sessions = fleet.sessions()?;
                Ok(json!({ "sessions": sessions.iter().map(summarise_session).collect::<Vec<_>>() }))
            }),
            "list_external_sessions" => self.read(|fleet| {
                Ok(json!({ "sessions": fleet.external_sessions()? }))
            }),
            "session_status" => {
                let id = require_session(&arguments)?;
                self.read(move |fleet| {
                    let sessions = fleet.sessions()?;
                    let found = sessions
                        .iter()
                        .find(|session| session.get("id").and_then(Value::as_str) == Some(&id));
                    match found {
                        Some(session) => Ok(summarise_session(session)),
                        None => Err(format!("no session {id}")),
                    }
                })
            }
            "session_last_output" => {
                let id = require_session(&arguments)?;
                let lines = arguments
                    .get("lines")
                    .and_then(Value::as_u64)
                    .map(|lines| (lines as usize).clamp(1, MAX_OUTPUT_LINES))
                    .unwrap_or(DEFAULT_OUTPUT_LINES);
                self.read(move |fleet| {
                    Ok(json!({
                        "session": id,
                        "lines": lines,
                        "output": fleet.last_output(&id, lines)?,
                        "note": "Terminal output only. This server does not expose transcripts.",
                    }))
                })
            }
            "fleet_cost" => self.read(|fleet| {
                let admission = fleet.admission()?;
                let reporting = admission
                    .get("sessions_reporting_cost")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                Ok(json!({
                    "aggregate_cost_usd": admission.get("aggregate_cost_usd"),
                    "sessions_reporting_cost": reporting,
                    "pricing_version": admission.get("pricing_version"),
                    // Zero sessions reporting means the spend is unknown, not
                    // zero; saying so stops a sibling agent quoting a false 0.
                    "cost_is_known": reporting > 0,
                }))
            }),
            "send_to_session" => self.mutate(name, &arguments, |fleet, id, arguments| {
                let text = arguments
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "send_to_session needs text".to_owned())?;
                // Bracketed paste, matching what the GUI sends, so a multi-line
                // prompt is not interpreted a line at a time.
                fleet.write_session(id, &format!("\x1b[200~{text}\x1b[201~\r"))?;
                Ok(json!({ "sent": true, "session": id }))
            }),
            "stop_session" => self.mutate(name, &arguments, |fleet, id, _| {
                fleet.kill_session(id)?;
                Ok(json!({ "stopped": true, "session": id }))
            }),
            other => Err((INVALID_PARAMS, format!("unknown tool {other}"))),
        }
    }

    fn read(
        &self,
        body: impl FnOnce(&F) -> Result<Value, String>,
    ) -> Result<Value, (i64, String)> {
        match body(&self.fleet) {
            Ok(value) => Ok(tool_result(value)),
            // A fleet that cannot be read is a tool error, not a protocol error:
            // the call was well-formed and the client can retry.
            Err(error) => Ok(tool_error(&error)),
        }
    }

    fn mutate(
        &self,
        tool: &str,
        arguments: &Value,
        body: impl FnOnce(&F, &str, &Value) -> Result<Value, String>,
    ) -> Result<Value, (i64, String)> {
        let id = require_session(arguments)?;
        let token = string_argument(arguments, "token");
        if let Some(refusal) = self.gate.refuse(token.as_deref(), &id) {
            self.fleet.log_mutation(tool, &id, false, &refusal);
            return Ok(tool_error(&refusal));
        }
        match body(&self.fleet, &id, arguments) {
            Ok(value) => {
                self.fleet.log_mutation(tool, &id, true, "");
                Ok(tool_result(value))
            }
            Err(error) => {
                self.fleet.log_mutation(tool, &id, true, &error);
                Ok(tool_error(&error))
            }
        }
    }
}

/// Only the fields a sibling agent needs. Deliberately a whitelist: a `Session`
/// grows over time, and forwarding it wholesale is how transcript-adjacent data
/// eventually leaks through a tool that was reviewed once.
fn summarise_session(session: &Value) -> Value {
    let field = |name: &str| session.get(name).cloned().unwrap_or(Value::Null);
    json!({
        "id": field("id"),
        "name": field("name"),
        "agent": field("agent"),
        "cwd": field("cwd"),
        "branch": field("branch"),
        "status": field("status"),
        "phase": field("phase"),
        "health": field("health"),
        "model": field("model"),
        "effort": field("effort"),
        "pinned": field("pinned"),
        "unread": field("unread"),
        "restarts": field("restarts"),
        "rate_limit": field("rate_limit"),
        "tool_progress": field("tool_progress"),
        "cost_usd": field("cost_usd"),
    })
}

fn tool_descriptors(gate: &WriteGate) -> Vec<Value> {
    TOOLS
        .iter()
        // A disabled mutating tool is not advertised at all: offering a tool
        // that always refuses invites a model to keep retrying it.
        .filter(|tool| !tool.mutating || gate.is_enabled())
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
                "annotations": {
                    "readOnlyHint": !tool.mutating,
                    "destructiveHint": tool.mutating,
                },
            })
        })
        .collect()
}

fn require_session(arguments: &Value) -> Result<String, (i64, String)> {
    string_argument(arguments, "session")
        .filter(|id| !id.trim().is_empty())
        .ok_or((INVALID_PARAMS, "this tool needs a session id".to_owned()))
}

fn string_argument(arguments: &Value, name: &str) -> Option<String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// MCP tool results are content blocks, and a failing tool sets `isError`
/// rather than returning a JSON-RPC error — the call itself succeeded.
fn tool_result(value: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
        "structuredContent": value,
        "isError": false,
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn success_response(id: Value, result: Value) -> String {
    serde_json::to_string(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        .unwrap_or_default()
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_else(|_| {
        format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":{INTERNAL_ERROR},"message":"response could not be encoded"}}}}"#)
    })
}

/// Strip ANSI escape sequences and other control bytes from terminal output.
///
/// Scrollback is raw pty bytes. Forwarding those into a tool result hands a
/// model a payload full of escape sequences, and hands whatever renders that
/// result a chance to act on them — so the text is reduced to what a human
/// would have seen before it leaves this process.
pub fn strip_terminal_control(text: &str) -> String {
    const ESC: char = '\u{1b}';
    const BEL: char = '\u{7}';
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ESC => match chars.next() {
                // CSI: parameters and intermediates, then one final byte.
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC and the other string-argument introducers run until BEL
                // or ST. An unterminated one consumes the rest, which is the
                // safe direction: better to drop text than to emit a sequence.
                Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                    while let Some(next) = chars.next() {
                        if next == BEL {
                            break;
                        }
                        if next == ESC && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // A two-character escape; its second byte is already consumed.
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            // Carriage returns are how a TUI overwrites a line; keeping them
            // would make one rendered line look like several.
            '\r' => {}
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {}
            c => out.push(c),
        }
    }
    out
}
