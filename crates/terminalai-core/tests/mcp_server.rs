//! The read-mostly MCP server.
//!
//! Most of these are about the boundary rather than the protocol: this is the
//! one surface where another model gets to ask the fleet questions, and a
//! mistake here is a remote shell rather than a wrong answer.

use std::cell::RefCell;

use serde_json::{json, Value};
use terminalai_core::mcp::{FleetAccess, McpServer, WriteGate, MAX_OUTPUT_LINES, PROTOCOL_VERSION};

#[derive(Default)]
struct Fake {
    mutations: RefCell<Vec<(String, String, bool, String)>>,
    writes: RefCell<Vec<(String, String)>>,
    kills: RefCell<Vec<String>>,
    fail_reads: bool,
}

impl FleetAccess for Fake {
    fn sessions(&self) -> Result<Vec<Value>, String> {
        if self.fail_reads {
            return Err("daemon is unavailable".to_owned());
        }
        Ok(vec![json!({
            "id": "s0001",
            "name": "shop",
            "agent": "claude",
            "cwd": "C:/repos/shop",
            "status": "working",
            "phase": "working",
            "health": "healthy",
            "model": "opus",
            "cost_usd": 1.25,
            // Fields a summary must not forward, present on purpose.
            "last_line": "secret operator prompt text",
            "status_history": [{"kind": "pty-output"}],
            "resume_id": "abc-123",
        })])
    }

    fn external_sessions(&self) -> Result<Vec<Value>, String> {
        Ok(vec![json!({ "agent": "codex", "pid": 4242, "state": "live" })])
    }

    fn admission(&self) -> Result<Value, String> {
        Ok(json!({
            "aggregate_cost_usd": 0.0,
            "sessions_reporting_cost": 0,
            "pricing_version": "litellm@bf1a8fe",
        }))
    }

    fn last_output(&self, id: &str, max_lines: usize) -> Result<String, String> {
        Ok(format!("{id}: last {max_lines} lines"))
    }

    fn write_session(&self, id: &str, data: &str) -> Result<(), String> {
        self.writes
            .borrow_mut()
            .push((id.to_owned(), data.to_owned()));
        Ok(())
    }

    fn kill_session(&self, id: &str) -> Result<(), String> {
        self.kills.borrow_mut().push(id.to_owned());
        Ok(())
    }

    fn log_mutation(&self, tool: &str, session: &str, allowed: bool, detail: &str) {
        self.mutations.borrow_mut().push((
            tool.to_owned(),
            session.to_owned(),
            allowed,
            detail.to_owned(),
        ));
    }
}

fn call(server: &mut McpServer<Fake>, method: &str, params: Value) -> Value {
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let line = server
        .handle_line(&request.to_string())
        .expect("a request with an id must be answered");
    serde_json::from_str(&line).expect("valid JSON response")
}

fn call_tool(server: &mut McpServer<Fake>, name: &str, arguments: Value) -> Value {
    call(server, "tools/call", json!({ "name": name, "arguments": arguments }))["result"].clone()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

fn read_only() -> McpServer<Fake> {
    McpServer::new(Fake::default(), WriteGate::default())
}

fn writable() -> McpServer<Fake> {
    McpServer::new(
        Fake::default(),
        WriteGate::new("s3cret", ["s0001".to_owned()]),
    )
}

#[test]
fn initialize_reports_the_protocol_and_the_boundary() {
    let mut server = read_only();
    let response = call(&mut server, "initialize", json!({}));
    assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(response["result"]["serverInfo"]["name"], "terminalai");
    // The client is told the shape of the boundary before it calls anything.
    let instructions = response["result"]["instructions"].as_str().expect("text");
    assert!(instructions.contains("read-only"), "{instructions}");
    assert!(instructions.contains("transcripts are never exposed"), "{instructions}");
}

#[test]
fn a_notification_is_never_answered() {
    // Replying to a notification is a protocol error some clients treat as
    // fatal, and `notifications/initialized` arrives on every connection.
    let mut server = read_only();
    for method in ["notifications/initialized", "notifications/cancelled"] {
        let line = json!({ "jsonrpc": "2.0", "method": method }).to_string();
        assert_eq!(server.handle_line(&line), None, "{method}");
    }
}

#[test]
fn a_response_frame_is_not_mistaken_for_a_request() {
    let mut server = read_only();
    let line = json!({ "jsonrpc": "2.0", "id": 7, "result": {} }).to_string();
    // It has an id but no method, so it is answered as an invalid request
    // rather than silently dispatched.
    let answer: Value =
        serde_json::from_str(&server.handle_line(&line).expect("answered")).expect("json");
    assert_eq!(answer["error"]["code"], -32600);
}

#[test]
fn malformed_json_is_reported_rather_than_dropped() {
    let mut server = read_only();
    let answer: Value = serde_json::from_str(&server.handle_line("{not json").expect("answered"))
        .expect("json");
    assert_eq!(answer["error"]["code"], -32700);
}

#[test]
fn a_read_only_server_advertises_no_mutating_tools() {
    // Advertising a tool that always refuses invites a model to keep retrying.
    let mut server = read_only();
    let tools = call(&mut server, "tools/list", json!({}))["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"list_sessions"));
    assert!(names.contains(&"fleet_cost"));
    assert!(!names.contains(&"send_to_session"), "{names:?}");
    assert!(!names.contains(&"stop_session"), "{names:?}");
    assert!(tools.iter().all(|tool| tool["annotations"]["readOnlyHint"] == json!(true)));
}

#[test]
fn a_writable_server_advertises_them_and_marks_them_destructive() {
    let mut server = writable();
    let tools = call(&mut server, "tools/list", json!({}))["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    let send = tools
        .iter()
        .find(|tool| tool["name"] == json!("send_to_session"))
        .expect("send_to_session is advertised");
    assert_eq!(send["annotations"]["readOnlyHint"], json!(false));
    assert_eq!(send["annotations"]["destructiveHint"], json!(true));
    // The token is a required argument, so a model cannot omit it by accident.
    let required = send["inputSchema"]["required"].as_array().expect("required");
    assert!(required.contains(&json!("token")), "{required:?}");
}

#[test]
fn session_listings_never_carry_transcript_adjacent_fields() {
    // The whole reason the summary is a whitelist: `Session` grows, and
    // forwarding it wholesale is how the operator's own prompts eventually
    // reach another model.
    let mut server = read_only();
    let result = call_tool(&mut server, "list_sessions", json!({}));
    let text = serde_json::to_string(&result).expect("text");
    assert!(text.contains("s0001"), "the summary still identifies the session");
    assert!(text.contains("working"));
    for leaked in ["secret operator prompt text", "status_history", "resume_id"] {
        assert!(!text.contains(leaked), "{leaked} must not be forwarded");
    }
}

#[test]
fn last_output_is_bounded_and_says_what_it_is_not() {
    let mut server = read_only();
    let result = call_tool(
        &mut server,
        "session_last_output",
        json!({ "session": "s0001", "lines": 10_000 }),
    );
    let structured = &result["structuredContent"];
    assert_eq!(
        structured["lines"].as_u64().expect("lines") as usize,
        MAX_OUTPUT_LINES,
        "an unbounded request must be clamped, not honoured"
    );
    assert!(structured["note"]
        .as_str()
        .expect("note")
        .contains("does not expose transcripts"));
}

#[test]
fn an_unknown_session_is_an_error_not_an_empty_answer() {
    let mut server = read_only();
    let result = call_tool(&mut server, "session_status", json!({ "session": "s9999" }));
    assert!(is_error(&result));
}

#[test]
fn a_fleet_with_no_reported_cost_says_the_cost_is_unknown() {
    // Otherwise a sibling agent quotes $0.00 as though it were measured.
    let mut server = read_only();
    let result = call_tool(&mut server, "fleet_cost", json!({}));
    assert_eq!(result["structuredContent"]["cost_is_known"], json!(false));
    assert_eq!(result["structuredContent"]["sessions_reporting_cost"], json!(0));
}

#[test]
fn a_read_only_server_refuses_every_mutating_call_and_records_it() {
    let mut server = read_only();
    for (tool, arguments) in [
        ("send_to_session", json!({ "session": "s0001", "text": "hi", "token": "s3cret" })),
        ("stop_session", json!({ "session": "s0001", "token": "s3cret" })),
    ] {
        let result = call_tool(&mut server, tool, arguments);
        assert!(is_error(&result), "{tool} must refuse");
    }
    // Refusals are logged too: an attempt to write into a read-only fleet is
    // exactly what an operator needs to see.
    let fake = server.into_fleet();
    let mutations = fake.mutations.borrow();
    assert_eq!(mutations.len(), 2);
    assert!(mutations.iter().all(|(_, _, allowed, _)| !allowed));
    assert!(fake.writes.borrow().is_empty());
    assert!(fake.kills.borrow().is_empty());
}

#[test]
fn a_wrong_token_is_refused_and_the_reason_distinguishes_it_from_a_wrong_session() {
    // An operator who opted a session in and still cannot write needs to know
    // which half is wrong.
    let mut server = writable();
    let bad_token = call_tool(
        &mut server,
        "send_to_session",
        json!({ "session": "s0001", "text": "hi", "token": "wrong" }),
    );
    assert!(is_error(&bad_token));
    assert!(bad_token["content"][0]["text"]
        .as_str()
        .expect("text")
        .contains("token"));

    let not_opted_in = call_tool(
        &mut server,
        "send_to_session",
        json!({ "session": "s0002", "text": "hi", "token": "s3cret" }),
    );
    assert!(is_error(&not_opted_in));
    let message = not_opted_in["content"][0]["text"].as_str().expect("text");
    assert!(message.contains("s0002"), "{message}");
    assert!(message.contains("opted in"), "{message}");

    assert!(server.into_fleet().writes.borrow().is_empty());
}

#[test]
fn a_missing_token_is_refused_even_for_an_opted_in_session() {
    let mut server = writable();
    let result = call_tool(
        &mut server,
        "send_to_session",
        json!({ "session": "s0001", "text": "hi" }),
    );
    assert!(is_error(&result));
    assert!(server.into_fleet().writes.borrow().is_empty());
}

#[test]
fn an_authorised_write_reaches_the_session_as_bracketed_paste() {
    let mut server = writable();
    let result = call_tool(
        &mut server,
        "send_to_session",
        json!({ "session": "s0001", "text": "status?", "token": "s3cret" }),
    );
    assert!(!is_error(&result), "{result}");

    let fake = server.into_fleet();
    let writes = fake.writes.borrow();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "s0001");
    // Bracketed paste, matching the GUI, so a multi-line prompt is not
    // interpreted a line at a time.
    assert_eq!(writes[0].1, "\u{1b}[200~status?\u{1b}[201~\r");
    // Every mutating call is recorded, not only the refused ones.
    let mutations = fake.mutations.borrow();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0].0, "send_to_session");
    assert!(mutations[0].2, "an allowed call is recorded as allowed");
}

#[test]
fn an_authorised_stop_reaches_the_session() {
    let mut server = writable();
    let result = call_tool(
        &mut server,
        "stop_session",
        json!({ "session": "s0001", "token": "s3cret" }),
    );
    assert!(!is_error(&result), "{result}");
    assert_eq!(server.into_fleet().kills.borrow().as_slice(), ["s0001"]);
}

#[test]
fn an_unreadable_fleet_is_a_tool_error_not_a_protocol_error() {
    // The call was well-formed; the client can retry. A JSON-RPC error would
    // read as "this tool does not exist".
    let mut server = McpServer::new(
        Fake {
            fail_reads: true,
            ..Default::default()
        },
        WriteGate::default(),
    );
    let response = call(
        &mut server,
        "tools/call",
        json!({ "name": "list_sessions", "arguments": {} }),
    );
    assert!(response.get("error").is_none(), "{response}");
    assert!(is_error(&response["result"]));
}

#[test]
fn unknown_methods_and_tools_are_named_rather_than_ignored() {
    let mut server = read_only();
    let unknown_method = call(&mut server, "resources/read", json!({}));
    assert_eq!(unknown_method["error"]["code"], -32601);

    let unknown_tool = call(
        &mut server,
        "tools/call",
        json!({ "name": "rm_rf", "arguments": {} }),
    );
    assert_eq!(unknown_tool["error"]["code"], -32602);
}

#[test]
fn tool_metadata_cannot_vary_with_what_the_fleet_contains() {
    // Tool poisoning is instructions smuggled into metadata and re-executed on
    // every call. It is only possible if metadata comes from somewhere mutable,
    // so the property to hold is that the advertised tools are byte-identical
    // whatever the fleet holds — stronger than grepping for known strings,
    // which would miss a field added later.
    let mut populated = read_only();
    let mut empty = McpServer::new(
        Fake {
            fail_reads: true,
            ..Default::default()
        },
        WriteGate::default(),
    );
    let from_populated = call(&mut populated, "tools/list", json!({}))["result"].clone();
    let from_empty = call(&mut empty, "tools/list", json!({}))["result"].clone();
    assert_eq!(from_populated, from_empty);

    // And nothing session-derived appears in it. `s0001` is excluded from this
    // list on purpose: it is a static example inside an argument description,
    // not data read from the fleet.
    let text = serde_json::to_string(&from_populated).expect("text");
    for session_data in ["C:/repos/shop", "secret operator prompt text", "abc-123", "opus"] {
        assert!(!text.contains(session_data), "{session_data} leaked into tool metadata");
    }
}
