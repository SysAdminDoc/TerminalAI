//! Normalized agent hook events.
//!
//! Hooks are deliberately represented as data before they reach the registry.
//! Claude and Codex use different field names, but both can report the small
//! lifecycle/attention vocabulary the fleet needs. The probe translates each
//! agent's stdin payload into this type and the daemon transports it over the
//! authenticated local pipe.

use std::path::PathBuf;

use crate::agent::Agent;
use crate::session::ToolProgress;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookEvent {
    pub agent: Agent,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub signal: HookSignal,
    /// A countable plan, when the tool call carried one. `None` means the agent
    /// exposed no plan on this event — the row keeps whatever it had rather than
    /// inventing a number, and renders an em dash if it never had one.
    #[serde(default)]
    pub progress: Option<ToolProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HookSignal {
    SessionStart,
    Stop,
    PreToolUse,
    PostToolUse,
    Notification { notification: HookNotification },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookNotification {
    PermissionPrompt,
    IdlePrompt,
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum HookParseError {
    #[error("invalid hook JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported hook event {0:?}")]
    UnsupportedEvent(String),
}

#[derive(Debug, serde::Deserialize)]
struct RawHook {
    #[serde(default, alias = "thread_id")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default, alias = "event_name")]
    hook_event_name: Option<String>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    notification_type: Option<String>,
    #[serde(default, rename = "type")]
    notification_kind: Option<String>,
    /// Claude's `tool_input`, Codex's `arguments`. Carries the plan for the
    /// planning tools; ignored for everything else.
    #[serde(default, alias = "arguments", alias = "tool_response")]
    tool_input: Option<serde_json::Value>,
    /// Some payloads put the plan at the top level rather than inside the input.
    #[serde(default)]
    plan: Option<serde_json::Value>,
    #[serde(default)]
    todos: Option<serde_json::Value>,
}

/// Statuses both agents use for a finished plan item.
const COMPLETED_STATES: [&str; 3] = ["completed", "complete", "done"];

/// Extract a countable plan from a tool payload.
///
/// Claude Code's `TodoWrite` carries `tool_input.todos[]` with a `status` of
/// `pending`/`in_progress`/`completed`; Codex's `update_plan` carries `plan[]`
/// with the same vocabulary. Anything else — a payload with no plan, an empty
/// plan, or entries without a status — yields `None` rather than a zero, because
/// "0/0" reads as progress that was measured.
fn parse_tool_progress(raw: &RawHook) -> Option<ToolProgress> {
    let candidates = [
        raw.todos.as_ref(),
        raw.plan.as_ref(),
        raw.tool_input.as_ref().and_then(|input| input.get("todos")),
        raw.tool_input.as_ref().and_then(|input| input.get("plan")),
        raw.tool_input.as_ref().and_then(|input| input.get("steps")),
    ];
    let items = candidates
        .into_iter()
        .flatten()
        .find_map(|value| value.as_array().filter(|items| !items.is_empty()))?;

    let mut completed = 0u32;
    let mut total = 0u32;
    for item in items {
        let status = item
            .get("status")
            .or_else(|| item.get("state"))
            .and_then(serde_json::Value::as_str)?;
        total = total.saturating_add(1);
        if COMPLETED_STATES.contains(&normalize(status).as_str()) {
            completed = completed.saturating_add(1);
        }
    }
    (total > 0).then_some(ToolProgress { completed, total })
}

/// Translate the JSON stdin contract used by Claude/Codex into the daemon's
/// small, agent-neutral event model. Unknown notification kinds are retained as
/// `Other` so a new upstream notification never breaks hook delivery.
pub fn parse_hook(agent: Agent, input: &str) -> Result<HookEvent, HookParseError> {
    let raw: RawHook = serde_json::from_str(input)?;
    let progress = parse_tool_progress(&raw);
    let event_name = raw.hook_event_name.or(raw.event).unwrap_or_default();
    let normalized = normalize(&event_name);
    let notification_name = raw.notification_type.or(raw.notification_kind);
    let signal = match normalized.as_str() {
        "sessionstart" | "session_start" => HookSignal::SessionStart,
        "sessionend" | "session_end" | "stop" => HookSignal::Stop,
        "pretooluse" | "pre_tool_use" => HookSignal::PreToolUse,
        "posttooluse" | "post_tool_use" => HookSignal::PostToolUse,
        "notification" | "permissionrequest" | "permission_request" => HookSignal::Notification {
            notification: parse_notification(notification_name.as_deref()),
        },
        "" if notification_name.is_some() => HookSignal::Notification {
            notification: parse_notification(notification_name.as_deref()),
        },
        other if is_permission_notification(other) => HookSignal::Notification {
            notification: HookNotification::PermissionPrompt,
        },
        other if is_idle_notification(other) => HookSignal::Notification {
            notification: HookNotification::IdlePrompt,
        },
        other => return Err(HookParseError::UnsupportedEvent(other.into())),
    };
    Ok(HookEvent {
        agent,
        session_id: raw
            .session_id
            .filter(|session_id| !session_id.trim().is_empty()),
        cwd: raw.cwd.or_else(|| std::env::current_dir().ok()),
        signal,
        progress,
    })
}

fn parse_notification(value: Option<&str>) -> HookNotification {
    let Some(value) = value.map(normalize) else {
        return HookNotification::Other;
    };
    if is_permission_notification(&value) {
        HookNotification::PermissionPrompt
    } else if is_idle_notification(&value) {
        HookNotification::IdlePrompt
    } else {
        HookNotification::Other
    }
}

fn is_permission_notification(value: &str) -> bool {
    value.contains("permission") || value.contains("approval")
}

fn is_idle_notification(value: &str) -> bool {
    value.contains("idle")
        || value.contains("awaiting")
        || value.contains("waiting")
        || value.contains("complete")
        || value.contains("finished")
}

fn normalize(value: &str) -> String {
    value
        .trim()
        .replace(['-', ' ', '.'], "_")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_notification_maps_to_attention_state() {
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"Notification","notification_type":"permission_prompt"}"#,
        )
        .expect("notification");
        assert_eq!(event.session_id.as_deref(), Some("cc-1"));
        assert_eq!(
            event.signal,
            HookSignal::Notification {
                notification: HookNotification::PermissionPrompt
            }
        );
    }

    #[test]
    fn codex_notify_payload_uses_thread_id_and_idle_alias() {
        let event = parse_hook(
            Agent::Codex,
            r#"{"thread_id":"cx-1","type":"agent-turn-complete"}"#,
        )
        .expect("notify payload");
        assert_eq!(event.session_id.as_deref(), Some("cx-1"));
        assert_eq!(
            event.signal,
            HookSignal::Notification {
                notification: HookNotification::IdlePrompt
            }
        );
    }

    #[test]
    fn unknown_notification_kind_is_forward_compatible() {
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"Notification","notification_type":"future_event"}"#,
        )
        .expect("future notification");
        assert_eq!(
            event.signal,
            HookSignal::Notification {
                notification: HookNotification::Other
            }
        );
    }

    #[test]
    fn claude_todowrite_payload_yields_a_countable_plan() {
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"PostToolUse","tool_name":"TodoWrite",
                "tool_input":{"todos":[
                  {"content":"a","status":"completed","activeForm":"doing a"},
                  {"content":"b","status":"in_progress","activeForm":"doing b"},
                  {"content":"c","status":"pending","activeForm":"doing c"}]}}"#,
        )
        .expect("todo payload");
        assert_eq!(
            event.progress,
            Some(ToolProgress {
                completed: 1,
                total: 3
            })
        );
    }

    #[test]
    fn codex_update_plan_payload_yields_the_same_shape() {
        let event = parse_hook(
            Agent::Codex,
            r#"{"thread_id":"cx-1","event":"postToolUse","arguments":{"plan":[
                  {"step":"a","status":"completed"},
                  {"step":"b","status":"completed"},
                  {"step":"c","status":"in_progress"}],"explanation":"x"}}"#,
        )
        .expect("plan payload");
        assert_eq!(
            event.progress,
            Some(ToolProgress {
                completed: 2,
                total: 3
            })
        );
    }

    #[test]
    fn a_tool_call_without_a_plan_reports_no_progress() {
        // An em dash is the honest render for an agent that exposes no plan;
        // "0/0" would read as a measurement that was taken.
        for payload in [
            r#"{"session_id":"cc-1","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
            r#"{"session_id":"cc-1","hook_event_name":"PostToolUse","tool_input":{"todos":[]}}"#,
            r#"{"session_id":"cc-1","hook_event_name":"PostToolUse","tool_input":{"todos":[{"content":"a"}]}}"#,
            r#"{"session_id":"cc-1","hook_event_name":"SessionStart"}"#,
        ] {
            let event = parse_hook(Agent::Claude, payload).expect("payload");
            assert_eq!(event.progress, None, "payload wrongly reported a plan: {payload}");
        }
    }

    #[test]
    fn unsupported_lifecycle_event_is_rejected() {
        let error = parse_hook(
            Agent::Codex,
            r#"{"thread_id":"cx-1","event":"BeforeCompaction"}"#,
        )
        .expect_err("unsupported event");
        assert!(matches!(error, HookParseError::UnsupportedEvent(_)));
    }
}
