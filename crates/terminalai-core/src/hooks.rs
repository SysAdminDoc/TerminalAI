//! Normalized agent hook events.
//!
//! Hooks are deliberately represented as data before they reach the registry.
//! Claude and Codex use different field names, but both can report the small
//! lifecycle/attention vocabulary the fleet needs. The probe translates each
//! agent's stdin payload into this type and the daemon transports it over the
//! authenticated local pipe.

use std::path::PathBuf;

use crate::agent::Agent;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookEvent {
    pub agent: Agent,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub signal: HookSignal,
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
}

/// Translate the JSON stdin contract used by Claude/Codex into the daemon's
/// small, agent-neutral event model. Unknown notification kinds are retained as
/// `Other` so a new upstream notification never breaks hook delivery.
pub fn parse_hook(agent: Agent, input: &str) -> Result<HookEvent, HookParseError> {
    let raw: RawHook = serde_json::from_str(input)?;
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
    fn unsupported_lifecycle_event_is_rejected() {
        let error = parse_hook(
            Agent::Codex,
            r#"{"thread_id":"cx-1","event":"BeforeCompaction"}"#,
        )
        .expect_err("unsupported event");
        assert!(matches!(error, HookParseError::UnsupportedEvent(_)));
    }
}
