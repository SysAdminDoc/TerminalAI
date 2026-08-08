//! Normalized agent hook events.
//!
//! Hooks are deliberately represented as data before they reach the registry.
//! Claude and Codex use different field names, but both can report the small
//! lifecycle/attention vocabulary the fleet needs. The probe translates each
//! agent's stdin payload into this type and the daemon transports it over the
//! authenticated local pipe.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::agent::Agent;
use crate::launch::is_valid_resume_id;
use crate::session::ToolProgress;

/// The result of trying to attribute one normalized hook to a supervised row.
///
/// A token is an authentication secret, not a durable agent-instance identity:
/// Claude Code teammates may inherit the lead's environment. When more than
/// one live row satisfies the same authenticated fallback, refusing the event
/// is safer than letting map order decide which row changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAttribution {
    Matched(crate::session::SessionId),
    Unknown,
    Ambiguous(Vec<crate::session::SessionId>),
}

impl HookAttribution {
    pub fn matched_id(&self) -> Option<&crate::session::SessionId> {
        match self {
            Self::Matched(id) => Some(id),
            Self::Unknown | Self::Ambiguous(_) => None,
        }
    }

    pub fn is_matched(&self) -> bool {
        matches!(self, Self::Matched(_))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookAgentDeliveryStatus {
    /// At least one syntactically valid hook reached the daemon in this
    /// daemon lifetime. Configuration alone never sets this flag.
    pub observed: bool,
    pub observed_events: u64,
    pub matched_events: u64,
    pub ambiguous_events: u64,
    pub unmatched_events: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookDeliveryStatus {
    pub claude: HookAgentDeliveryStatus,
    pub codex: HookAgentDeliveryStatus,
}

impl HookDeliveryStatus {
    pub fn for_agent(&self, agent: Agent) -> &HookAgentDeliveryStatus {
        match agent {
            Agent::Claude => &self.claude,
            Agent::Codex => &self.codex,
        }
    }
}

/// Shared, bounded proof that hook traffic reached this daemon. It deliberately
/// stores counters only; session ids and payloads remain in the registry/log
/// path rather than becoming preflight-readable data.
#[derive(Debug, Default)]
pub struct HookDeliveryState {
    status: Mutex<HookDeliveryStatus>,
}

impl HookDeliveryState {
    pub fn observe(&self, agent: Agent, attribution: &HookAttribution) {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let agent_status = match agent {
            Agent::Claude => &mut status.claude,
            Agent::Codex => &mut status.codex,
        };
        agent_status.observed = true;
        agent_status.observed_events = agent_status.observed_events.saturating_add(1);
        match attribution {
            HookAttribution::Matched(_) => {
                agent_status.matched_events = agent_status.matched_events.saturating_add(1);
            }
            HookAttribution::Ambiguous(_) => {
                agent_status.ambiguous_events = agent_status.ambiguous_events.saturating_add(1);
            }
            HookAttribution::Unknown => {
                agent_status.unmatched_events = agent_status.unmatched_events.saturating_add(1);
            }
        }
    }

    pub fn snapshot(&self) -> HookDeliveryStatus {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

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
    /// What the agent is asking permission to do, when this event is a
    /// permission prompt carrying enough to say.
    ///
    /// Alongside the signal rather than inside it, exactly as `progress` is:
    /// the signal answers what happened, and both of these are details that
    /// ride on whichever event happened to carry them. A permission prompt
    /// reaches the fleet as `Notification { PermissionPrompt }` from several
    /// different event names, and folding the detail into one of them would
    /// leave the others blank.
    #[serde(default)]
    pub approval: Option<HookApprovalRequest>,
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
    /// The agent changed its working directory.
    ///
    /// The row's folder and branch describe where the session *is*, and an
    /// agent that moves invalidates both. Unhandled, the row went on naming a
    /// directory the session had left and a branch belonging to it — quietly,
    /// and for the rest of the session.
    CwdChanged,
    /// The agent is about to create its own worktree and will accept a path.
    ///
    /// The one hook that lets this supervisor own a checkout the agent made
    /// rather than discovering it later as a stray.
    WorktreeCreate,
    /// The agent removed a worktree.
    ///
    /// The counterpart to [`Self::WorktreeCreate`]: the supervisor answers the
    /// creation with a placement and then surveys the root for strays, but
    /// nothing told it when the agent cleaned one up — so the row went on naming
    /// a checkout that no longer existed.
    WorktreeRemove,
    Notification { notification: HookNotification },
    /// A provider quota is refusing work. Reported by the agent, never inferred
    /// from a session going quiet — silence is indistinguishable from a long
    /// tool call, and this is the state the fleet header counts.
    RateLimited { limit: HookRateLimit },
    /// A quota table with room left. Positive evidence the window is open, and
    /// the only thing that clears a limit — but it also carries the headroom
    /// the agent reported, which is what lets the fleet warn *before* work
    /// stops rather than only after.
    RateLimitCleared { limit: HookRateLimit },
    Unknown { event: String },
}

/// Longest tool name kept from a permission request. A name is a short
/// identifier; anything longer is not one, and this is agent-supplied.
pub const MAX_APPROVAL_TOOL_CHARS: usize = 64;
/// Longest argument summary kept.
///
/// A `Write` tool's input is an entire file and an `Edit`'s is two of them.
/// The summary exists to let an operator recognise what is being asked, not to
/// reproduce it — the pane still has the full request, and an unbounded copy
/// would ride on every fleet snapshot, which is sent on every status change.
pub const MAX_APPROVAL_SUMMARY_CHARS: usize = 300;

/// What an agent is asking permission to do.
///
/// Both fields are optional and independently so: an agent can name a tool
/// without describing it, and a payload can carry arguments under a name this
/// build does not recognise. An empty request still means the session is
/// blocked — it just means the fleet cannot say on what, which is worth
/// showing as an absence rather than filling in.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookApprovalRequest {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

/// Argument keys worth showing on their own, in the order they are preferred.
///
/// A `Bash` request is its `command` and nothing else; rendering the whole
/// object around it buries the one line the operator is deciding about. Keys
/// not listed fall back to compact JSON, which is still better than nothing.
const APPROVAL_SUMMARY_KEYS: &[&str] = &[
    "command",
    "file_path",
    "path",
    "url",
    "pattern",
    "query",
    "prompt",
];

/// Read the tool and a bounded summary of its arguments from a hook payload.
fn parse_approval_request(raw: &RawHook) -> HookApprovalRequest {
    let tool = raw
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| truncate_chars(name, MAX_APPROVAL_TOOL_CHARS));
    let summary = raw.tool_input.as_ref().and_then(approval_summary);
    HookApprovalRequest { tool, summary }
}

/// Render an argument object as one readable line.
///
/// Public so the Codex app-server path can summarise its own approval params
/// with the same rule: two renderings of "what is being asked" that disagreed
/// would be two features wearing one name.
pub fn approval_summary_of(input: &serde_json::Value) -> Option<String> {
    approval_summary(input)
}

fn approval_summary(input: &serde_json::Value) -> Option<String> {
    if let Some(object) = input.as_object() {
        for key in APPROVAL_SUMMARY_KEYS {
            if let Some(value) = object.get(*key) {
                let text = match value {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let text = collapse_whitespace(&text);
                if !text.is_empty() {
                    return Some(truncate_chars(&text, MAX_APPROVAL_SUMMARY_CHARS));
                }
            }
        }
    }
    let rendered = collapse_whitespace(&input.to_string());
    // "null" and "{}" are the JSON encoder describing an absence. Showing them
    // would put a value in a field that has none.
    if rendered.is_empty() || rendered == "null" || rendered == "{}" {
        return None;
    }
    Some(truncate_chars(&rendered, MAX_APPROVAL_SUMMARY_CHARS))
}

/// One line, no runs of blanks. A multi-line heredoc in a `command` would
/// otherwise break the row it is rendered into.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    // Never a byte slice: this is agent-supplied text and slicing mid-codepoint
    // panics.
    text.chars().take(limit).collect()
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

/// One `Notification` hook, classified.
///
/// The variants are the `notification_type` values Claude Code documents, and
/// they are matched **by name** rather than by looking for a word inside the
/// string. Substring matching is what let `agent_completed` land on `IdlePrompt`
/// by coincidence — it contains "complete" — while `agent_needs_input`, whose
/// entire meaning is "this session is waiting for you", matched nothing and was
/// dropped. A taxonomy that classifies by accident classifies the next new value
/// by accident too.
///
/// The heuristics are kept for names this list does not cover, because Codex and
/// future versions send their own spellings, and an unrecognised name is logged
/// rather than silently filed as [`Self::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookNotification {
    PermissionPrompt,
    IdlePrompt,
    /// `agent_needs_input`, added in Claude Code 2.1.198. The one notification
    /// whose whole meaning is the thing this tool exists to surface.
    AgentNeedsInput,
    /// `elicitation_dialog` — an MCP server is asking the operator something
    /// through the agent. Attention for the same reason a permission prompt is.
    ElicitationDialog,
    /// `agent_completed`. The run ended.
    AgentCompleted,
    /// `auth_success`. Credentials that were reported expired are good again.
    AuthSuccess,
    /// Documented, deliberately uninteresting, or unrecognised. Never acted on.
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
    /// Which tool the agent is using or asking to use. Dropped before
    /// 2026-08-07, which is why a blocked row could not say what it was
    /// blocked on.
    #[serde(default, alias = "toolName", alias = "tool")]
    tool_name: Option<String>,
    /// Claude's `tool_input`, Codex's `arguments`. Carries the plan for the
    /// planning tools, and the request for a permission prompt.
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
    // Read before `raw` is partially moved below.
    let requested = parse_approval_request(&raw);
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
        // goes quiet proves nothing. The table travels with it: it is the same
        // measurement, and discarding it here is what left the fleet unable to
        // say anything about quota until a session had already stopped.
        Some(limit) => HookSignal::RateLimitCleared { limit },
        None => parse_lifecycle_signal(&normalized, &event_name, notification_name.as_deref()),
    };
    // Only for a prompt that is actually blocking. Attaching the tool to every
    // PreToolUse would put a stale "asking to run X" on rows that are not
    // asking for anything.
    let approval = matches!(
        signal,
        HookSignal::PermissionRequest
            | HookSignal::Notification {
                notification: HookNotification::PermissionPrompt
            }
    )
    .then_some(requested)
    .filter(|request| request != &HookApprovalRequest::default());
    Ok(HookEvent {
        agent,
        session_id: raw
            .session_id
            .filter(|session_id| is_valid_resume_id(session_id)),
        cwd: raw.cwd.or(fallback_cwd),
        signal,
        progress,
        approval,
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
        "cwdchanged" | "cwd_changed" => HookSignal::CwdChanged,
        "worktreecreate" | "worktree_create" => HookSignal::WorktreeCreate,
        "worktreeremove" | "worktree_remove" => HookSignal::WorktreeRemove,
        "subagentstop" | "subagent_stop" => HookSignal::SubagentStop,
        "notification" => HookSignal::Notification {
            notification: parse_notification(notification_name),
        },
        // The event name is itself the kind. Sharing the `notification` arm
        // meant a payload that named the event `PermissionRequest` and carried
        // no `notification_type` parsed as `Other` — which the registry ignores
        // entirely, so a session blocked on a prompt went on reading as
        // Working. An explicit `notification_type` still wins, because a
        // payload that names a kind knows better than the event name does.
        "permissionrequest" | "permission_request" => HookSignal::Notification {
            notification: match parse_notification(notification_name) {
                HookNotification::Other => HookNotification::PermissionPrompt,
                named => named,
            },
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
    // Documented names first, exactly. Anything reached by the heuristics below
    // is a guess, and a guess must never outrank a value the vendor publishes.
    match value.as_str() {
        "permission_prompt" => return HookNotification::PermissionPrompt,
        "idle_prompt" => return HookNotification::IdlePrompt,
        "agent_needs_input" => return HookNotification::AgentNeedsInput,
        "elicitation_dialog" => return HookNotification::ElicitationDialog,
        "agent_completed" => return HookNotification::AgentCompleted,
        "auth_success" => return HookNotification::AuthSuccess,
        // Documented and deliberately not acted on: these report that an
        // elicitation the operator already saw has been answered, which changes
        // nothing about whether the session needs them.
        "elicitation_complete" | "elicitation_response" => return HookNotification::Other,
        _ => {}
    }
    if is_permission_notification(&value) {
        HookNotification::PermissionPrompt
    } else if is_idle_notification(&value) {
        HookNotification::IdlePrompt
    } else {
        // Logged rather than silently filed. A new `notification_type` is how
        // this taxonomy falls behind the platform, and the log is the only place
        // that can say it happened.
        tracing::debug!(notification = %value, "unrecognised notification type");
        HookNotification::Other
    }
}

/// Every `notification_type` this build recognises by name.
///
/// Exposed so a test can walk it rather than restating the list, and so adding
/// a variant without a name is a compile error rather than a silent gap.
pub const DOCUMENTED_NOTIFICATIONS: [(&str, HookNotification); 8] = [
    ("permission_prompt", HookNotification::PermissionPrompt),
    ("idle_prompt", HookNotification::IdlePrompt),
    ("auth_success", HookNotification::AuthSuccess),
    ("elicitation_dialog", HookNotification::ElicitationDialog),
    ("elicitation_complete", HookNotification::Other),
    ("elicitation_response", HookNotification::Other),
    ("agent_needs_input", HookNotification::AgentNeedsInput),
    ("agent_completed", HookNotification::AgentCompleted),
];

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

    /// Every documented `notification_type` is classified by its own name.
    ///
    /// Walks the table rather than restating it, so a variant added without a
    /// name is a compile error and a name added without a test is impossible.
    #[test]
    fn every_documented_notification_type_is_recognised_by_name() {
        for (name, expected) in DOCUMENTED_NOTIFICATIONS {
            let event = parse_hook(
                Agent::Claude,
                &format!(
                    r#"{{"session_id":"cc-1","hook_event_name":"Notification","notification_type":"{name}"}}"#
                ),
            )
            .unwrap_or_else(|error| panic!("{name} should parse: {error}"));
            assert_eq!(
                event.signal,
                HookSignal::Notification {
                    notification: expected
                },
                "{name}"
            );
        }
    }

    #[test]
    fn the_two_names_that_used_to_be_classified_by_accident_are_not_anymore() {
        // `agent_completed` contains "complete", so the idle heuristic caught it
        // by coincidence and would have caught anything else containing the
        // word. `agent_needs_input` — the one notification whose entire meaning
        // is "this session is waiting for you" — matched nothing and was
        // dropped. Both are now decided by name, and this is the assertion that
        // fails if either arm is removed and the heuristics take over again.
        let needs_input = notification_for("agent_needs_input");
        assert_eq!(needs_input, HookNotification::AgentNeedsInput);
        assert_ne!(needs_input, HookNotification::Other, "it used to be dropped");

        assert_eq!(
            notification_for("agent_completed"),
            HookNotification::AgentCompleted
        );
        // The three elicitation values all contain neither "permission" nor
        // "idle", but two of them contain words the idle heuristic looks for.
        assert_eq!(
            notification_for("elicitation_complete"),
            HookNotification::Other,
            "an answered elicitation is not the operator's turn"
        );
        assert_eq!(
            notification_for("elicitation_dialog"),
            HookNotification::ElicitationDialog
        );
    }

    #[test]
    fn an_unknown_notification_type_is_still_classified_by_the_heuristics() {
        // Codex and future versions send their own spellings, so dropping the
        // fallback would trade one gap for another.
        assert_eq!(
            notification_for("tool-permission-request"),
            HookNotification::PermissionPrompt
        );
        assert_eq!(
            notification_for("awaiting-user"),
            HookNotification::IdlePrompt
        );
        assert_eq!(notification_for("something-new"), HookNotification::Other);
    }

    /// The classification of one `notification_type`, through the real parser.
    fn notification_for(name: &str) -> HookNotification {
        let event = parse_hook(
            Agent::Claude,
            &format!(
                r#"{{"session_id":"cc-1","hook_event_name":"Notification","notification_type":"{name}"}}"#
            ),
        )
        .unwrap_or_else(|error| panic!("{name} should parse: {error}"));
        match event.signal {
            HookSignal::Notification { notification } => notification,
            other => panic!("{name} produced {other:?}"),
        }
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
        let HookSignal::RateLimitCleared { limit } = event.signal else {
            panic!("a window with room left is not a refusal");
        };
        // The headroom survives the classification; it is the number the header
        // needs in order to warn before the window closes.
        assert_eq!(limit.used_percent, Some(63.5));
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
    fn a_permission_prompt_says_which_tool_and_with_what() {
        // "This session needs approval" is not a decision anybody can make.
        // Before this, the tool name was dropped by the parser entirely.
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#,
        )
        .expect("parses");
        let approval = event.approval.expect("a permission prompt carries its request");
        assert_eq!(approval.tool.as_deref(), Some("Bash"));
        assert_eq!(approval.summary.as_deref(), Some("rm -rf build"));
    }

    #[test]
    fn an_event_named_permission_request_is_a_permission_prompt() {
        // It shared an arm with the generic `Notification` event, so a payload
        // that named the event and carried no `notification_type` parsed as
        // `Other` — which the registry ignores, leaving a session blocked on a
        // prompt reading as Working.
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"PermissionRequest","tool_name":"Bash"}"#,
        )
        .expect("parses");
        assert_eq!(
            event.signal,
            HookSignal::Notification {
                notification: HookNotification::PermissionPrompt
            }
        );
    }

    #[test]
    fn a_named_notification_kind_still_beats_the_event_name() {
        // A payload that names a kind knows better than the event name does.
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"PermissionRequest","notification_type":"idle_prompt"}"#,
        )
        .expect("parses");
        assert_eq!(
            event.signal,
            HookSignal::Notification {
                notification: HookNotification::IdlePrompt
            }
        );
    }

    #[test]
    fn an_ordinary_tool_call_carries_no_pending_request() {
        // Attaching the tool to every PreToolUse would leave a stale "asking to
        // run X" on rows that are not asking for anything.
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        )
        .expect("parses");
        assert_eq!(event.approval, None);
    }

    #[test]
    fn a_prompt_that_names_no_tool_still_reports_as_blocked() {
        // An agent can ask without saying what for. The absence is the answer;
        // inventing a tool name would be worse than showing nothing.
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","hook_event_name":"Notification","notification_type":"permission_prompt"}"#,
        )
        .expect("parses");
        assert!(
            matches!(
                event.signal,
                HookSignal::Notification {
                    notification: HookNotification::PermissionPrompt
                }
            ),
            "still a permission prompt: {:?}",
            event.signal
        );
        assert_eq!(event.approval, None, "nothing to say, so nothing is said");
    }

    #[test]
    fn the_summary_is_the_argument_that_matters_not_the_object_around_it() {
        // A Bash request is its command. Rendering the whole object buries the
        // one line the operator is deciding about.
        let summary = |input: &str| {
            approval_summary(&serde_json::from_str::<serde_json::Value>(input).expect("json"))
        };
        assert_eq!(
            summary(r#"{"command":"git push --force","description":"push","timeout":120}"#)
                .as_deref(),
            Some("git push --force")
        );
        assert_eq!(
            summary(r#"{"file_path":"C:/repos/x/src/main.rs","content":"..."}"#).as_deref(),
            Some("C:/repos/x/src/main.rs")
        );
        // Nothing recognised still beats nothing at all.
        assert_eq!(
            summary(r#"{"unfamiliar":"value"}"#).as_deref(),
            Some(r#"{"unfamiliar":"value"}"#)
        );
        // An encoder describing an absence is not a value.
        assert_eq!(summary("null"), None);
        assert_eq!(summary("{}"), None);
    }

    #[test]
    fn a_multiline_command_becomes_one_line() {
        // A heredoc would otherwise break the row it is rendered into.
        let event = parse_hook(
            Agent::Claude,
            "{\"session_id\":\"cc-1\",\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"cat <<EOF\\n  one\\n\\n  two\\nEOF\"}}",
        )
        .expect("parses");
        let summary = event.approval.expect("request").summary.expect("summary");
        assert_eq!(summary, "cat <<EOF one two EOF");
        assert!(!summary.contains('\n'));
    }

    #[test]
    fn an_enormous_request_is_bounded_without_splitting_a_character() {
        // A Write tool's input is an entire file, and this rides on a row sent
        // to the window on every status change.
        let body = "é".repeat(5_000);
        let payload = serde_json::json!({
            "session_id": "cc-1",
            "hook_event_name": "PermissionRequest",
            "tool_name": "W".repeat(500),
            "tool_input": { "file_path": body },
        })
        .to_string();
        let event = parse_hook(Agent::Claude, &payload).expect("parses");
        let approval = event.approval.expect("request");
        assert_eq!(
            approval.tool.expect("tool").chars().count(),
            MAX_APPROVAL_TOOL_CHARS
        );
        assert_eq!(
            approval.summary.expect("summary").chars().count(),
            MAX_APPROVAL_SUMMARY_CHARS
        );
    }

    #[test]
    fn a_quota_report_does_not_hide_the_lifecycle_event_it_rode_in_on() {
        // Only events actually carrying a quota table are diverted; everything
        // else must still parse as its lifecycle signal.
        let event = parse_hook(Agent::Codex, r#"{"event_name":"PreToolUse"}"#).expect("parses");
        assert_eq!(event.signal, HookSignal::PreToolUse);
    }
}
