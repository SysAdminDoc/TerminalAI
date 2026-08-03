//! Additive types for the experimental Codex app-server transport.
//!
//! Codex's app-server speaks newline-delimited JSON-RPC over stdio. The
//! protocol currently omits the JSON-RPC `jsonrpc` member on the wire, but
//! accepting it when present keeps this boundary compatible with ordinary
//! JSON-RPC fixtures and future transports. Typed events cover the signals
//! TerminalAI can act on today; unknown messages retain their method and
//! parameters so an upstream schema change cannot silently discard data.

use serde_json::{json, Map, Value};

use crate::hooks::HookEvent;

pub type RpcId = Value;

// `Eq` is not derivable: a reported quota carries a `used_percent` float.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", content = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    Hook(HookEvent),
    AppServer(AppServerEvent),
}

impl From<HookEvent> for AgentEvent {
    fn from(event: HookEvent) -> Self {
        Self::Hook(event)
    }
}

impl From<AppServerEvent> for AgentEvent {
    fn from(event: AppServerEvent) -> Self {
        Self::AppServer(event)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event_kind", rename_all = "snake_case")]
pub enum AppServerEvent {
    ThreadStatusChanged {
        thread_id: String,
        status: AppServerThreadStatus,
    },
    TokenUsageUpdated {
        thread_id: String,
        usage: AppServerTokenUsage,
    },
    ApprovalRequested {
        request_id: RpcId,
        thread_id: String,
        turn_id: Option<String>,
        kind: AppServerApprovalKind,
        method: String,
        params: Value,
    },
    Unknown {
        method: String,
        params: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppServerThreadStatus {
    pub kind: String,
    pub active_flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppServerTokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    pub model_context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppServerApprovalKind {
    CommandExecution,
    FileChange,
    Permissions,
    UserInput,
    McpElicitation,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppServerMessage {
    Response {
        id: RpcId,
        result: Option<Value>,
        error: Option<Value>,
    },
    Notification {
        event: AppServerEvent,
    },
    Request {
        id: RpcId,
        event: AppServerEvent,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AppServerParseError {
    #[error("invalid app-server JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid app-server message: {0}")]
    InvalidMessage(String),
    #[error("unsupported JSON-RPC version {0:?}")]
    UnsupportedVersion(String),
}

/// Parse one newline-delimited app-server message.
pub fn parse_message(line: &str) -> Result<AppServerMessage, AppServerParseError> {
    let value: Value = serde_json::from_str(line)?;
    let object = value.as_object().ok_or_else(|| {
        AppServerParseError::InvalidMessage("message must be a JSON object".into())
    })?;
    validate_version(object)?;

    if let Some(method) = object.get("method").and_then(Value::as_str) {
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        if let Some(id) = object.get("id") {
            let event = parse_event(method, params, Some(id));
            return Ok(AppServerMessage::Request {
                id: id.clone(),
                event,
            });
        }
        let event = parse_event(method, params, None);
        return Ok(AppServerMessage::Notification { event });
    }

    if let Some(id) = object.get("id") {
        return Ok(AppServerMessage::Response {
            id: id.clone(),
            result: object.get("result").cloned(),
            error: object.get("error").cloned(),
        });
    }

    Err(AppServerParseError::InvalidMessage(
        "message has neither method nor id".into(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

impl AppServerRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            id,
            method: method.into(),
            params,
        }
    }

    pub fn initialize(
        id: u64,
        name: impl Into<String>,
        title: impl Into<String>,
        version: impl Into<String>,
        experimental_api: bool,
    ) -> Self {
        Self::new(
            id,
            "initialize",
            json!({
                "clientInfo": {
                    "name": name.into(),
                    "title": title.into(),
                    "version": version.into(),
                },
                "capabilities": { "experimentalApi": experimental_api },
            }),
        )
    }

    pub fn steer(
        id: u64,
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            "turn/steer",
            json!({
                "threadId": thread_id.into(),
                "input": [{ "type": "text", "text": text.into() }],
                "expectedTurnId": turn_id.into(),
            }),
        )
    }

    pub fn interrupt(id: u64, thread_id: impl Into<String>, turn_id: impl Into<String>) -> Self {
        Self::new(
            id,
            "turn/interrupt",
            json!({
                "threadId": thread_id.into(),
                "turnId": turn_id.into(),
            }),
        )
    }

    pub fn encode_line(&self) -> Result<String, serde_json::Error> {
        encode_line(&json!({
            "id": self.id,
            "method": self.method,
            "params": self.params,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerNotification {
    pub method: String,
    pub params: Value,
}

impl AppServerNotification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
        }
    }

    pub fn initialized() -> Self {
        Self::new("initialized", json!({}))
    }

    pub fn encode_line(&self) -> Result<String, serde_json::Error> {
        encode_line(&json!({
            "method": self.method,
            "params": self.params,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerResponse {
    pub id: RpcId,
    pub result: Value,
}

impl AppServerResponse {
    pub fn new(id: RpcId, result: Value) -> Self {
        Self { id, result }
    }

    pub fn encode_line(&self) -> Result<String, serde_json::Error> {
        encode_line(&json!({ "id": self.id, "result": self.result }))
    }
}

fn encode_line(value: &Value) -> Result<String, serde_json::Error> {
    Ok(format!("{}\n", serde_json::to_string(value)?))
}

fn validate_version(object: &Map<String, Value>) -> Result<(), AppServerParseError> {
    let Some(version) = object.get("jsonrpc") else {
        return Ok(());
    };
    match version.as_str() {
        Some("2.0") => Ok(()),
        Some(other) => Err(AppServerParseError::UnsupportedVersion(other.into())),
        None => Err(AppServerParseError::UnsupportedVersion(version.to_string())),
    }
}

fn parse_event(method: &str, params: Value, request_id: Option<&Value>) -> AppServerEvent {
    match method {
        "thread/status/changed" => {
            parse_status_event(&params).unwrap_or_else(|| unknown(method, params))
        }
        "thread/tokenUsage/updated" => {
            parse_usage_event(&params).unwrap_or_else(|| unknown(method, params))
        }
        _ => {
            if approval_kind(method).is_some() {
                parse_approval_event(method, params, request_id.cloned())
            } else {
                unknown(method, params)
            }
        }
    }
}

fn parse_status_event(params: &Value) -> Option<AppServerEvent> {
    let object = params.as_object()?;
    let thread_id = string_field(object, &["threadId", "thread_id"])?;
    let status_value = object.get("status")?;
    let status_object = status_value.as_object();
    let kind = status_object
        .and_then(|status| string_field(status, &["type", "kind"]))
        .or_else(|| status_value.as_str().map(str::to_owned))?;
    let active_flags = status_object
        .and_then(|status| {
            status
                .get("activeFlags")
                .or_else(|| status.get("active_flags"))
        })
        .and_then(Value::as_array)
        .map(|flags| {
            flags
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(AppServerEvent::ThreadStatusChanged {
        thread_id,
        status: AppServerThreadStatus { kind, active_flags },
    })
}

fn parse_usage_event(params: &Value) -> Option<AppServerEvent> {
    let object = params.as_object()?;
    let thread_id = string_field(object, &["threadId", "thread_id"])?;
    let token_usage = object
        .get("tokenUsage")
        .or_else(|| object.get("token_usage"))
        .or_else(|| object.get("usage"))
        .unwrap_or(params);
    let usage_object = token_usage.as_object()?;
    let values = usage_object
        .get("total")
        .or_else(|| usage_object.get("last"))
        .or_else(|| usage_object.get("usage"))
        .unwrap_or(token_usage);
    let values = values.as_object()?;
    let has_usage = [
        "inputTokens",
        "input_tokens",
        "cachedInputTokens",
        "cached_input_tokens",
        "outputTokens",
        "output_tokens",
        "reasoningOutputTokens",
        "reasoning_output_tokens",
        "totalTokens",
        "total_tokens",
    ]
    .iter()
    .any(|name| values.contains_key(*name));
    if !has_usage {
        return None;
    }
    let input_tokens = number_field(values, &["inputTokens", "input_tokens"]);
    let cached_input_tokens = number_field(values, &["cachedInputTokens", "cached_input_tokens"]);
    let output_tokens = number_field(values, &["outputTokens", "output_tokens"]);
    let reasoning_output_tokens = number_field(
        values,
        &["reasoningOutputTokens", "reasoning_output_tokens"],
    );
    let total_tokens = number_field(values, &["totalTokens", "total_tokens"])
        .max(input_tokens.saturating_add(output_tokens));
    let model_context_window = optional_number_field(
        usage_object,
        &["modelContextWindow", "model_context_window"],
    );
    Some(AppServerEvent::TokenUsageUpdated {
        thread_id,
        usage: AppServerTokenUsage {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            reasoning_output_tokens,
            total_tokens,
            model_context_window,
        },
    })
}

fn parse_approval_event(method: &str, params: Value, request_id: Option<Value>) -> AppServerEvent {
    let object = params.as_object();
    let thread_id = object
        .and_then(|object| string_field(object, &["threadId", "thread_id"]))
        .unwrap_or_default();
    let turn_id = object.and_then(|object| string_field(object, &["turnId", "turn_id"]));
    AppServerEvent::ApprovalRequested {
        request_id: request_id.unwrap_or(Value::Null),
        thread_id,
        turn_id,
        kind: approval_kind(method).unwrap_or_else(|| AppServerApprovalKind::Other(method.into())),
        method: method.into(),
        params,
    }
}

fn approval_kind(method: &str) -> Option<AppServerApprovalKind> {
    Some(match method {
        "item/commandExecution/requestApproval" => AppServerApprovalKind::CommandExecution,
        "item/fileChange/requestApproval" => AppServerApprovalKind::FileChange,
        "item/permissions/requestApproval" => AppServerApprovalKind::Permissions,
        "item/tool/requestUserInput" => AppServerApprovalKind::UserInput,
        "mcpServer/elicitation/request" => AppServerApprovalKind::McpElicitation,
        _ => return None,
    })
}

fn unknown(method: &str, params: Value) -> AppServerEvent {
    AppServerEvent::Unknown {
        method: method.into(),
        params,
    }
}

fn string_field(object: &Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_str).map(str::to_owned))
}

fn number_field(object: &Map<String, Value>, names: &[&str]) -> u64 {
    optional_number_field(object, names).unwrap_or_default()
}

fn optional_number_field(object: &Map<String, Value>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_thread_status_notification() {
        let message = parse_message(
            r#"{"method":"thread/status/changed","params":{"threadId":"thr-1","status":{"type":"active","activeFlags":["waitingOnApproval"]}}}"#,
        )
        .expect("status notification");
        assert!(matches!(
            message,
            AppServerMessage::Notification {
                event: AppServerEvent::ThreadStatusChanged { thread_id, status }
            } if thread_id == "thr-1"
                && status.kind == "active"
                && status.active_flags == vec!["waitingOnApproval"]
        ));
    }

    #[test]
    fn parses_total_token_usage_notification() {
        let message = parse_message(
            r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","tokenUsage":{"total":{"inputTokens":12,"cachedInputTokens":3,"outputTokens":7,"reasoningOutputTokens":2,"totalTokens":19},"modelContextWindow":100}}}"#,
        )
        .expect("usage notification");
        assert!(matches!(
            message,
            AppServerMessage::Notification {
                event: AppServerEvent::TokenUsageUpdated { thread_id, usage }
            } if thread_id == "thr-1"
                && usage.input_tokens == 12
                && usage.cached_input_tokens == 3
                && usage.output_tokens == 7
                && usage.reasoning_output_tokens == 2
                && usage.total_tokens == 19
                && usage.model_context_window == Some(100)
        ));
    }

    #[test]
    fn parses_server_approval_request_and_keeps_params() {
        let message = parse_message(
            r#"{"id":9,"method":"item/commandExecution/requestApproval","params":{"itemId":"item-1","threadId":"thr-1","turnId":"turn-1","reason":"needs access"}}"#,
        )
        .expect("approval request");
        assert!(matches!(
            message,
            AppServerMessage::Request {
                id: Value::Number(id),
                event: AppServerEvent::ApprovalRequested {
                    request_id,
                    thread_id,
                    turn_id,
                    kind,
                    params,
                    ..
                }
            } if id.as_u64() == Some(9)
                && request_id.as_u64() == Some(9)
                && thread_id == "thr-1"
                && turn_id.as_deref() == Some("turn-1")
                && kind == AppServerApprovalKind::CommandExecution
                && params["itemId"] == "item-1"
        ));
    }

    #[test]
    fn unknown_notifications_are_preserved() {
        let message = parse_message(
            r#"{"method":"future/event","params":{"threadId":"thr-1","value":true}}"#,
        )
        .expect("future notification");
        assert_eq!(
            message,
            AppServerMessage::Notification {
                event: AppServerEvent::Unknown {
                    method: "future/event".into(),
                    params: json!({"threadId":"thr-1","value":true}),
                }
            }
        );
    }

    #[test]
    fn steer_request_matches_current_wire_shape() {
        let request = AppServerRequest::steer(32, "thr-1", "turn-1", "focus tests");
        let value: Value =
            serde_json::from_str(&request.encode_line().expect("encode")).expect("request JSON");
        assert_eq!(value["id"], 32);
        assert_eq!(value["method"], "turn/steer");
        assert_eq!(value["params"]["threadId"], "thr-1");
        assert_eq!(value["params"]["expectedTurnId"], "turn-1");
        assert_eq!(value["params"]["input"][0]["text"], "focus tests");
        assert!(value.get("jsonrpc").is_none());
    }

    #[test]
    fn standard_json_rpc_version_is_accepted_but_other_versions_fail() {
        assert!(parse_message(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).is_ok());
        assert!(matches!(
            parse_message(r#"{"jsonrpc":"1.0","id":1,"result":{}}"#),
            Err(AppServerParseError::UnsupportedVersion(_))
        ));
    }
}
