//! Normalized agent hook events.
//!
//! Hooks are deliberately represented as data before they reach the registry.
//! Claude and Codex use different field names, but both can report the small
//! lifecycle/attention vocabulary the fleet needs. The probe translates each
//! agent's stdin payload into this type and the daemon transports it over the
//! authenticated local pipe.

use std::path::PathBuf;

use crate::agent::Agent;
use crate::launch::is_valid_resume_id;
use crate::session::ToolProgress;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HookSignal {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    Stop,
    StopFailure,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionRequest,
    PermissionDenied,
    PreCompact,
    PostCompact,
    SubagentStart,
    SubagentStop,
    Notification { notification: HookNotification },
    /// A provider quota is refusing work. Reported by the agent, never inferred
    /// from a session going quiet — silence is indistinguishable from a long
    /// tool call, and this is the state the fleet header counts.
    RateLimited { limit: HookRateLimit },
    /// A previously reported quota is over and the session can work again.
    RateLimitCleared,
    Unknown { event: String },
}

/// The rate-limit facts an agent reported, before they are stamped with a
/// receipt time and stored on the session.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookRateLimit {
    pub scope: String,
    #[serde(default)]
    pub used_percent: Option<f64>,
    #[serde(default)]
    pub window_minutes: Option<u64>,
    /// Seconds from receipt until the window resets, when the agent gives a
    /// relative delay. Absolute timestamps arrive as `resets_at_unix`.
    #[serde(default)]
    pub resets_in_seconds: Option<u64>,
    #[serde(default)]
    pub resets_at_unix: Option<u64>,
    #[serde(default)]
    pub plan: Option<String>,
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
    /// Codex's rollout puts the quota table under `token_count.rate_limits`;
    /// `account/rateLimits/read` returns the same shape at the top level.
    #[serde(default)]
    rate_limits: Option<serde_json::Value>,
    #[serde(default)]
    token_count: Option<serde_json::Value>,
    /// Claude's headless `system/api_retry` carries the category here.
    #[serde(default, alias = "error_type")]
    error: Option<serde_json::Value>,
    #[serde(default)]
    retry_after_seconds: Option<u64>,
    #[serde(default)]
    plan_type: Option<String>,
}

/// Read a Codex-shaped quota table into a normalized limit.
///
/// Codex reports several windows at once (`primary`, `secondary`, …), each with
/// its own `used_percent` and `resets_in_seconds`. The one that matters is the
/// window actually blocking work, so the most-consumed window wins; ties keep
/// the first, which is Codex's own ordering.
fn parse_codex_rate_limits(value: &serde_json::Value) -> Option<HookRateLimit> {
    let object = value.as_object()?;
    let mut best: Option<HookRateLimit> = None;
    for (scope, window) in object {
        let Some(window) = window.as_object() else {
            continue;
        };
        let used_percent = window.get("used_percent").and_then(serde_json::Value::as_f64);
        let candidate = HookRateLimit {
            scope: scope.clone(),
            used_percent,
            window_minutes: window
                .get("window_minutes")
                .and_then(serde_json::Value::as_u64),
            resets_in_seconds: window
                .get("resets_in_seconds")
                .and_then(serde_json::Value::as_u64),
            resets_at_unix: window
                .get("resets_at")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    window
                        .get("resets_at")
                        .and_then(serde_json::Value::as_str)
                        .and_then(parse_unix_timestamp)
                }),
            plan: None,
        };
        let better = match (&best, used_percent) {
            (None, _) => true,
            (Some(current), Some(percent)) => percent > current.used_percent.unwrap_or(0.0),
            (Some(_), None) => false,
        };
        if better {
            best = Some(candidate);
        }
    }
    best
}

/// RFC 3339 timestamps only, and only the epoch-seconds value. Returning `None`
/// for anything unparsed is deliberate: a wrong reset time is worse than none,
/// because it sends the operator back to a session the provider still refuses.
fn parse_unix_timestamp(value: &str) -> Option<u64> {
    let value = value.trim();
    // A bare integer string is the common case from JSON-RPC.
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let (date, rest) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let time = rest
        .split(['Z', '+'])
        .next()?
        .split('.')
        .next()?
        .to_string();
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil, Howard Hinnant's algorithm — the same one `chrono` uses.
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(seconds).ok()
}

/// True when a Claude retry/error category names a quota refusal rather than a
/// transient network fault. `overloaded` is included because it produces the
/// same operator-visible outcome: the session sits there doing nothing.
fn rate_limit_category(value: &str) -> Option<&'static str> {
    let normalized = normalize(value);
    if normalized.contains("rate_limit") || normalized.contains("quota") {
        Some("rate-limit")
    } else if normalized.contains("overloaded") {
        Some("overloaded")
    } else {
        None
    }
}

/// Pull a rate limit out of whichever shape the agent used.
fn parse_rate_limit(raw: &RawHook) -> Option<HookRateLimit> {
    let table = raw.rate_limits.as_ref().or_else(|| {
        raw.token_count
            .as_ref()
            .and_then(|counts| counts.get("rate_limits"))
    });
    if let Some(limit) = table.and_then(parse_codex_rate_limits) {
        return Some(HookRateLimit {
            plan: raw.plan_type.clone(),
            ..limit
        });
    }

    // Claude's shape: a category, optionally with a retry delay.
    let category = raw.error.as_ref().and_then(|error| {
        error
            .as_str()
            .or_else(|| error.get("type").and_then(serde_json::Value::as_str))
            .or_else(|| error.get("category").and_then(serde_json::Value::as_str))
    })?;
    let scope = rate_limit_category(category)?;
    Some(HookRateLimit {
        scope: scope.to_owned(),
        used_percent: None,
        window_minutes: None,
        resets_in_seconds: raw.retry_after_seconds,
        resets_at_unix: None,
        plan: raw.plan_type.clone(),
    })
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

/// Translate the JSON hook contract used by Claude/Codex into the daemon's
/// agent-neutral event model. Unknown event names are retained instead of
/// rejected so an upstream addition remains visible to the daemon and its
/// diagnostics stream.
///
/// Parse a hook without guessing which process's working directory should
/// identify it. Callers that own the hook transport may provide their own
/// fallback through [`parse_hook_in`].
pub fn parse_hook(agent: Agent, input: &str) -> Result<HookEvent, HookParseError> {
    parse_hook_in(agent, input, None)
}

/// Parse a hook with a caller-owned working-directory fallback.
///
/// The CLI adapter runs in the agent's directory and can safely pass that
/// directory here. HTTP hooks run inside the daemon, so they must pass `None`:
/// the daemon's working directory is not evidence about the hook's session.
pub fn parse_hook_in(
    agent: Agent,
    input: &str,
    fallback_cwd: Option<PathBuf>,
) -> Result<HookEvent, HookParseError> {
    let raw: RawHook = serde_json::from_str(input)?;
    let progress = parse_tool_progress(&raw);
    let rate_limit = parse_rate_limit(&raw);
    let event_name = raw.hook_event_name.or(raw.event).unwrap_or_default();
    let normalized = normalize(&event_name);
    let notification_name = raw.notification_type.clone().or(raw.notification_kind.clone());
    // Resolved before the lifecycle names because Codex attaches its quota
    // table to ordinary token-count events: keyed on the event name alone, a
    // limit would be dropped by whichever event happened to carry it.
    let signal = match rate_limit {
        Some(limit) if is_blocking_limit(&limit) => HookSignal::RateLimited { limit },
        // A quota table with room left is positive evidence the window reset —
        // the only thing that clears this state, since a session that simply
        // goes quiet proves nothing.
        Some(_) => HookSignal::RateLimitCleared,
        None => parse_lifecycle_signal(&normalized, &event_name, notification_name.as_deref()),
    };
    Ok(HookEvent {
        agent,
        session_id: raw
            .session_id
            .filter(|session_id| is_valid_resume_id(session_id)),
        cwd: raw.cwd.or(fallback_cwd),
        signal,
        progress,
    })
}

/// A limit is blocking when the provider is actually refusing work.
///
/// Claude reports a refusal directly, so its category is enough. Codex reports
/// its quota table continuously, including while there is room left, so a
/// consumed window is the only Codex evidence that work is being refused —
/// treating a partly-used window as blocking would put healthy sessions into an
/// attention state.
fn is_blocking_limit(limit: &HookRateLimit) -> bool {
    matches!(limit.scope.as_str(), "rate-limit" | "overloaded")
        || limit.used_percent.is_some_and(|percent| percent >= 100.0)
}

fn parse_lifecycle_signal(
    normalized: &str,
    event_name: &str,
    notification_name: Option<&str>,
) -> HookSignal {
    match normalized {
        "sessionstart" | "session_start" => HookSignal::SessionStart,
        "sessionend" | "session_end" => HookSignal::SessionEnd,
        "userpromptsubmit" | "user_prompt_submit" => HookSignal::UserPromptSubmit,
        "stop" => HookSignal::Stop,
        "stopfailure" | "stop_failure" => HookSignal::StopFailure,
        "pretooluse" | "pre_tool_use" => HookSignal::PreToolUse,
        "posttooluse" | "post_tool_use" => HookSignal::PostToolUse,
        "posttoolusefailure" | "post_tool_use_failure" => HookSignal::PostToolUseFailure,
        "permissiondenied" | "permission_denied" => HookSignal::PermissionDenied,
        "precompact" | "pre_compact" => HookSignal::PreCompact,
        "postcompact" | "post_compact" => HookSignal::PostCompact,
        "subagentstart" | "subagent_start" => HookSignal::SubagentStart,
        "subagentstop" | "subagent_stop" => HookSignal::SubagentStop,
        "notification" | "permissionrequest" | "permission_request" => HookSignal::Notification {
            notification: parse_notification(notification_name),
        },
        "" if notification_name.is_some() => HookSignal::Notification {
            notification: parse_notification(notification_name),
        },
        other if is_permission_notification(other) => HookSignal::Notification {
            notification: HookNotification::PermissionPrompt,
        },
        other if is_idle_notification(other) => HookSignal::Notification {
            notification: HookNotification::IdlePrompt,
        },
        _ => HookSignal::Unknown {
            event: event_name.trim().to_owned(),
        },
    }
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
    fn a_flag_like_session_id_is_dropped_at_hook_ingest() {
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"--dangerously-skip-permissions","hook_event_name":"SessionStart"}"#,
        )
        .expect("hook");
        assert_eq!(event.session_id, None);
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
            assert_eq!(
                event.progress, None,
                "payload wrongly reported a plan: {payload}"
            );
        }
    }

    #[test]
    fn unknown_lifecycle_event_is_retained() {
        let event = parse_hook(
            Agent::Codex,
            r#"{"thread_id":"cx-1","event":"BeforeCompaction"}"#,
        )
        .expect("unknown event");
        assert_eq!(
            event.signal,
            HookSignal::Unknown {
                event: "BeforeCompaction".into()
            }
        );
    }

    #[test]
    fn lifecycle_events_map_without_dropping_newer_variants() {
        for (name, expected) in [
            ("SessionEnd", HookSignal::SessionEnd),
            ("UserPromptSubmit", HookSignal::UserPromptSubmit),
            ("PostToolUseFailure", HookSignal::PostToolUseFailure),
            ("PermissionDenied", HookSignal::PermissionDenied),
            ("SubagentStart", HookSignal::SubagentStart),
            ("SubagentStop", HookSignal::SubagentStop),
            ("PreCompact", HookSignal::PreCompact),
            ("PostCompact", HookSignal::PostCompact),
        ] {
            let input = format!(r#"{{"hook_event_name":"{name}"}}"#);
            let event = parse_hook(Agent::Claude, &input).expect("lifecycle event");
            assert_eq!(event.signal, expected, "event {name}");
        }
    }

    #[test]
    fn codex_reports_a_consumed_window_as_a_refusal() {
        // Shape from a Codex rollout: rate_limits nested under token_count, one
        // entry per window.
        let input = r#"{
            "event_name": "token_count",
            "token_count": {
                "rate_limits": {
                    "primary": {"used_percent": 42.0, "window_minutes": 300, "resets_in_seconds": 900},
                    "secondary": {"used_percent": 100.0, "window_minutes": 10080, "resets_in_seconds": 7200}
                }
            },
            "plan_type": "pro"
        }"#;
        let event = parse_hook(Agent::Codex, input).expect("token count parses");
        let HookSignal::RateLimited { limit } = event.signal else {
            panic!("expected a rate limit, got {:?}", event.signal);
        };
        // The blocking window wins, not whichever key sorted first.
        assert_eq!(limit.scope, "secondary");
        assert_eq!(limit.window_minutes, Some(10080));
        assert_eq!(limit.resets_in_seconds, Some(7200));
        assert_eq!(limit.plan.as_deref(), Some("pro"));
    }

    #[test]
    fn a_window_with_room_left_is_not_a_refusal() {
        // Codex reports this table continuously. Treating any report as a limit
        // would park healthy sessions in an attention state.
        let input = r#"{
            "event_name": "token_count",
            "token_count": {"rate_limits": {"primary": {"used_percent": 63.5, "resets_in_seconds": 600}}}
        }"#;
        let event = parse_hook(Agent::Codex, input).expect("token count parses");
        assert_eq!(event.signal, HookSignal::RateLimitCleared);
    }

    #[test]
    fn claude_retry_categories_are_read_as_refusals() {
        for (category, scope) in [
            ("rate_limit_error", "rate-limit"),
            ("overloaded_error", "overloaded"),
            ("quota_exceeded", "rate-limit"),
        ] {
            let input = format!(
                r#"{{"event_name":"api_retry","error":{{"type":"{category}"}},"retry_after_seconds":120}}"#
            );
            let event = parse_hook(Agent::Claude, &input).expect("retry parses");
            let HookSignal::RateLimited { limit } = event.signal else {
                panic!("{category} should be a refusal, got {:?}", event.signal);
            };
            assert_eq!(limit.scope, scope);
            assert_eq!(limit.resets_in_seconds, Some(120));
        }
    }

    #[test]
    fn an_ordinary_transport_error_is_not_a_rate_limit() {
        // The reason the category list is an allowlist: a connection reset must
        // not park the row in a state that says "wait for your quota".
        let input = r#"{"event_name":"api_retry","error":{"type":"connection_error"}}"#;
        let event = parse_hook(Agent::Claude, input).expect("retry parses");
        assert!(
            !matches!(event.signal, HookSignal::RateLimited { .. }),
            "got {:?}",
            event.signal
        );
    }

    #[test]
    fn an_absolute_reset_timestamp_is_read_and_a_malformed_one_is_dropped() {
        // 2026-08-03T12:00:00Z. Checked against a known epoch value rather than
        // round-tripping our own arithmetic.
        assert_eq!(parse_unix_timestamp("2026-08-03T12:00:00Z"), Some(1785758400));
        assert_eq!(parse_unix_timestamp("1785758400"), Some(1785758400));
        // A reset time that cannot be read is dropped, never approximated: a
        // wrong one sends the operator back to a session still being refused.
        for malformed in ["soon", "2026-13-40T99:99:99Z", "", "2026-08-03"] {
            assert_eq!(parse_unix_timestamp(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn a_quota_report_does_not_hide_the_lifecycle_event_it_rode_in_on() {
        // Only events actually carrying a quota table are diverted; everything
        // else must still parse as its lifecycle signal.
        let event = parse_hook(Agent::Codex, r#"{"event_name":"PreToolUse"}"#).expect("parses");
        assert_eq!(event.signal, HookSignal::PreToolUse);
    }
}
