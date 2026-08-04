//! The fleet row model.
//!
//! A [`Session`] is what one line of the fleet list renders. It is deliberately
//! small and cheap to clone: the GUI re-renders the whole list on every status
//! change, and there may be thirty tracked sessions even while only a bounded
//! number of agent processes remain live.
//!
//! Status is *pushed* here by agent hooks and transcript tailing, never polled.
//! Polling a pty every two seconds is what every other multiplexer does, and it
//! is both wrong (you miss transitions between polls) and expensive.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::agent::Agent;
use crate::diagnostics::{StatusDiagnostic, StatusReason, StatusSource, MAX_STATUS_HISTORY};
use crate::launch::{Effort, LaunchSpec};

/// Maximum number of automatic restart attempts for one session. A session
/// that keeps failing after this limit stays failed until the operator revives
/// it explicitly.
pub const MAX_RESTARTS: u32 = 5;
pub const RESTART_BACKOFF_BASE: Duration = Duration::from_millis(250);
pub const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);

#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(seq: u64) -> Self {
        SessionId(format!("s{seq:04}"))
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the status dot shows. Ordering matters: the fleet list sorts by this
/// descending, so anything wanting the user floats to the top.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SessionStatus {
    /// Process gone.
    Exited,
    /// Waiting for an admission slot before a process is spawned.
    Queued,
    /// The process query failed; retain the session until its state is proven.
    Unknown,
    /// Started, nothing has happened yet.
    Starting,
    /// Waiting for the user to type.
    Idle,
    /// Model is generating.
    Thinking,
    /// Running a tool — the long tail where sessions get stuck.
    Working,
    /// The provider is refusing work until a quota window resets. Sorts above
    /// the working states because a limited session looks identical to a busy
    /// one from the outside and would otherwise read as a healthy fleet.
    RateLimited,
    /// Blocked on a permission prompt or a question. Demands attention.
    NeedsYou,
    /// Waiting for the user to answer an idle prompt.
    AwaitingInput,
    /// Waiting for an explicit permission decision.
    NeedsApproval,
}

impl SessionStatus {
    /// Colour token for the UI theme (Catppuccin Mocha names).
    pub fn colour(self) -> &'static str {
        match self {
            SessionStatus::Exited => "overlay0",
            SessionStatus::Queued => "overlay0",
            SessionStatus::Unknown => "overlay0",
            SessionStatus::Starting => "sapphire",
            SessionStatus::Idle => "surface2",
            SessionStatus::Thinking => "mauve",
            SessionStatus::Working => "yellow",
            SessionStatus::RateLimited => "red",
            SessionStatus::NeedsApproval => "peach",
            SessionStatus::AwaitingInput => "yellow",
            SessionStatus::NeedsYou => "peach",
        }
    }

    /// Whether the session still has a process behind it. Used to route hook
    /// events to a session, so a rate-limited session must stay live here — it
    /// is still running and will report again when its window resets.
    pub fn is_live(self) -> bool {
        !matches!(self, SessionStatus::Exited | SessionStatus::Queued)
    }

    /// Whether the session should hold an admission slot.
    ///
    /// Distinct from [`Self::is_live`]: a rate-limited session is running but
    /// cannot make progress until its window resets, so holding a slot would
    /// keep a queued session waiting behind work that provably is not happening.
    pub fn occupies_admission_slot(self) -> bool {
        self.is_live() && self != SessionStatus::RateLimited
    }
}

/// What the agent is doing, independent of whether its process is healthy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SessionPhase {
    Queued,
    Preparing,
    Unknown,
    Starting,
    TearingDown,
    Idle,
    Working,
    AwaitingInput,
    NeedsApproval,
    RateLimited,
    Backoff,
    Failed,
    Resurrectable,
}

/// Health of the process and its supervision boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionHealth {
    Queued,
    Starting,
    Healthy,
    Degraded,
    Failed,
}

/// Optional progress reported by an agent while it is carrying out a tool
/// plan. A missing value means that the agent has not exposed a countable
/// plan; the fleet row renders that as an em dash instead of inventing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolProgress {
    pub completed: u32,
    pub total: u32,
}

/// A provider quota window that is currently refusing work.
///
/// Every field is optional because the two agents report different subsets:
/// Codex's rollout carries `used_percent`, `window_minutes`, `resets_at` and
/// `plan_type`; Claude's retry events carry a category and sometimes a retry
/// delay. A missing `resets_at` renders as "reset time unknown" rather than
/// being guessed — the whole point of this state is that it is reported, not
/// inferred, and a fabricated reset time would send an operator away from a
/// session that is actually available.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RateLimit {
    /// Which quota tripped, verbatim from the agent (`primary`, `weekly`,
    /// `overloaded`, …). Shown so two limits are distinguishable in the row.
    pub scope: String,
    #[serde(default)]
    pub used_percent: Option<f64>,
    #[serde(default)]
    pub window_minutes: Option<u64>,
    /// When the window resets, if the agent said. Never computed from a guess.
    #[serde(default)]
    pub resets_at: Option<SystemTime>,
    #[serde(default)]
    pub plan: Option<String>,
    /// When this limit was reported, so a stale one can be aged out.
    pub reported_at: SystemTime,
}

impl RateLimit {
    /// True once the reported reset time has passed.
    ///
    /// A limit with no reset time never expires on a clock: it is cleared by the
    /// next event that proves the session is working again, because guessing
    /// would silently return the row to normal while the provider is still
    /// refusing.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.resets_at.is_some_and(|resets| now >= resets)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    Backoff(Duration),
    Failed,
}

/// One row of the fleet list.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent: Agent,
    /// User-supplied, else derived from the folder name.
    pub name: String,
    /// Secret carried only in this session's agent environment. It is not part
    /// of the row or store wire shape: hook authentication must never become a
    /// UI-readable session field.
    #[serde(skip)]
    pub(crate) hook_token: String,
    pub cwd: PathBuf,
    /// Git branch associated with the session, when the launch/runtime layer
    /// can identify one without guessing.
    #[serde(default)]
    pub branch: Option<String>,
    /// Deterministic service ports reserved for this session's environment.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// How many prompts are waiting their turn on this session.
    ///
    /// A count rather than the prompts themselves: a `Session` is cloned on
    /// every status change and sent to the window, and 32 prompts of up to a
    /// quarter of a megabyte each would make that the most expensive thing the
    /// fleet does. The prompts are fetched when the operator opens the queue.
    #[serde(default)]
    pub queued_prompts: usize,
    /// Why this session's queue stopped advancing, when it has.
    #[serde(default)]
    pub queue_paused: Option<crate::queue::PauseReason>,
    /// The private checkout this session was given, when it asked for one.
    ///
    /// Recorded on the session rather than the spec because it is a fact about
    /// what was created, not what was requested — and because cleaning it up
    /// after a daemon restart needs the path and branch that actually exist.
    #[serde(default)]
    pub worktree: Option<crate::worktree::Worktree>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub status: SessionStatus,
    pub phase: SessionPhase,
    pub health: SessionHealth,
    pub restarts: u32,
    pub last_exit_code: Option<u32>,
    pub backoff_until: Option<SystemTime>,
    pub state_since: SystemTime,
    pub pid: Option<u32>,
    /// Last line of agent output, trimmed for the row. Raw pty bytes, so it
    /// carries whatever escape sequences the TUI last drew.
    pub last_line: String,
    /// The last thing the agent actually said, read from its transcript.
    ///
    /// Distinct from `last_line` because they come from different places and
    /// one is strictly better: the pty carries a rendered TUI, where a redraw
    /// leaves box-drawing characters and cursor moves in the tail. The row
    /// prefers this when it exists and falls back to `last_line` when the
    /// transcript has not been read yet.
    #[serde(default)]
    pub last_message: Option<String>,
    /// Optional countable tool-plan progress for the fleet row.
    #[serde(default)]
    pub tool_progress: Option<ToolProgress>,
    /// Native session id, once the agent reports one. Enables resume and fork.
    pub resume_id: Option<String>,
    pub started_at: SystemTime,
    /// Retained as the raw-I/O status clock for existing clients. Supervision
    /// transitions use `state_since`.
    pub status_since: SystemTime,
    /// Accumulated spend, when the agent reports it.
    pub cost_usd: Option<f64>,
    /// Token totals read from the transcript, alongside the cost they priced.
    ///
    /// Carried as well as the dollar figure because they are different
    /// questions: cost answers "what did this spend", tokens answer "what is
    /// this session doing" — a run heavy in cache reads and one heavy in output
    /// cost similar amounts and behave nothing alike. Absent until a transcript
    /// has actually been read; zero would claim a session did no work.
    #[serde(default)]
    pub tokens: Option<crate::transcript::UsageTotals>,
    /// Set while a provider quota is refusing work for this session. Populated
    /// only from an explicit agent report — never from silence, which is
    /// indistinguishable from a long tool call.
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
    /// Set when the session enters an attention state and cleared when the user looks.
    pub unread: bool,
    /// Pinned sessions keep a live terminal grid even when not focused.
    pub pinned: bool,
    /// Bounded evidence for why the supervisor assigned each status.
    #[serde(default)]
    pub status_history: Vec<StatusDiagnostic>,
    /// Operator acknowledgement, bound to the diff state it was made against.
    ///
    /// A bare boolean was write-once: a session stayed marked reviewed while the
    /// agent kept changing files, which is exactly the "already handled" false
    /// negative a review queue exists to prevent. Holding the digest instead
    /// means the mark expires by itself the moment the tree moves.
    #[serde(default)]
    pub reviewed_digest: Option<String>,
}

impl Session {
    pub fn new(id: SessionId, spec: &LaunchSpec) -> Self {
        let now = SystemTime::now();
        let name = spec.name.clone().unwrap_or_else(|| folder_label(&spec.cwd));
        let ports = spec
            .environment
            .ports_for_session(&id.0)
            .unwrap_or_default();
        let mut session = Self {
            id,
            agent: spec.agent,
            name,
            hook_token: fresh_hook_token(),
            cwd: spec.cwd.clone(),
            branch: None,
            ports,
            queued_prompts: 0,
            queue_paused: None,
            worktree: None,
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            status: SessionStatus::Starting,
            phase: SessionPhase::Starting,
            health: SessionHealth::Starting,
            restarts: 0,
            last_exit_code: None,
            backoff_until: None,
            state_since: now,
            pid: None,
            last_line: String::new(),
            tool_progress: None,
            resume_id: None,
            started_at: now,
            status_since: now,
            cost_usd: None,
            tokens: None,
            last_message: None,
            rate_limit: None,
            unread: false,
            pinned: false,
            status_history: Vec::new(),
            reviewed_digest: None,
        };
        session.record_status_transition(None, SessionStatus::Starting, StatusSource::Launch, now);
        session
    }

    /// Apply a status transition, stamping the clock and raising the unread flag
    /// when the session starts wanting something.
    pub fn set_status(&mut self, status: SessionStatus) {
        self.set_status_from(status, StatusSource::Unknown);
    }

    /// Apply a status transition with the evidence source shown in diagnostics.
    pub fn set_status_from(&mut self, status: SessionStatus, source: StatusSource) {
        self.set_status_at(status, SystemTime::now(), source, false);
    }

    fn set_status_at(
        &mut self,
        status: SessionStatus,
        now: SystemTime,
        source: StatusSource,
        force_restamp: bool,
    ) {
        let previous = self.status;
        if !force_restamp && previous == status {
            return;
        }
        if previous != status
            && matches!(
                status,
                SessionStatus::NeedsApproval
                    | SessionStatus::AwaitingInput
                    | SessionStatus::NeedsYou
            )
        {
            self.unread = true;
        }
        self.status = status;
        self.status_since = now;
        self.state_since = now;
        self.phase = match status {
            SessionStatus::Queued => SessionPhase::Queued,
            SessionStatus::Unknown => SessionPhase::Unknown,
            SessionStatus::Starting => SessionPhase::Starting,
            SessionStatus::Idle => SessionPhase::Idle,
            SessionStatus::Thinking | SessionStatus::Working => SessionPhase::Working,
            SessionStatus::RateLimited => SessionPhase::RateLimited,
            SessionStatus::NeedsYou | SessionStatus::AwaitingInput => SessionPhase::AwaitingInput,
            SessionStatus::NeedsApproval => SessionPhase::NeedsApproval,
            SessionStatus::Exited => SessionPhase::Resurrectable,
        };
        self.health = match status {
            SessionStatus::Queued => SessionHealth::Queued,
            SessionStatus::Starting => SessionHealth::Starting,
            SessionStatus::Exited => SessionHealth::Degraded,
            _ if self.pid.is_some() => SessionHealth::Healthy,
            _ => SessionHealth::Degraded,
        };
        if previous != status {
            self.record_status_transition(Some(previous), status, source, now);
        }
    }

    fn record_status_transition(
        &mut self,
        from: Option<SessionStatus>,
        to: SessionStatus,
        source: StatusSource,
        at: SystemTime,
    ) {
        self.status_history.push(StatusDiagnostic {
            at,
            from,
            to,
            source,
            reason: StatusReason::for_transition(from, to, source, self.last_exit_code),
            detail: None,
        });
        let overflow = self.status_history.len().saturating_sub(MAX_STATUS_HISTORY);
        if overflow > 0 {
            self.status_history.drain(..overflow);
        }
    }

    /// Record a process that has been spawned. The agent may still be in its
    /// startup phase, but the supervision boundary is now healthy.
    pub fn mark_spawned_at(&mut self, pid: Option<u32>, now: SystemTime) {
        self.pid = pid;
        self.backoff_until = None;
        self.set_status_at(
            SessionStatus::Starting,
            now,
            StatusSource::ProcessStart,
            true,
        );
        self.health = SessionHealth::Healthy;
    }

    /// Keep the row visible while admission control waits for a live slot.
    pub fn mark_queued_at(&mut self, now: SystemTime) {
        self.pid = None;
        self.backoff_until = None;
        self.set_status_at(SessionStatus::Queued, now, StatusSource::Admission, true);
    }

    /// A failed process query is not proof of exit. Keep the PID and all
    /// session state while exposing an honest degraded state to the operator.
    pub fn mark_unknown_at(&mut self, now: SystemTime) {
        self.set_status_at(
            SessionStatus::Unknown,
            now,
            StatusSource::ProcessQuery,
            true,
        );
        self.health = SessionHealth::Degraded;
    }

    pub fn begin_restart_at(&mut self, now: SystemTime) {
        self.begin_restart_at_from(now, StatusSource::Supervisor);
    }

    pub fn begin_restart_at_from(&mut self, now: SystemTime, source: StatusSource) {
        self.pid = None;
        self.backoff_until = None;
        self.set_status_at(SessionStatus::Starting, now, source, true);
    }

    /// The environment hook worker is preparing the process boundary. Keep
    /// the public status at Starting while exposing the blocking phase.
    pub fn mark_preparing(&mut self) {
        self.phase = SessionPhase::Preparing;
        self.health = SessionHealth::Starting;
    }

    /// The agent has exited and its environment teardown is running on a
    /// worker. The prior terminal phase is restored when the worker completes.
    pub fn mark_tearing_down(&mut self) {
        self.phase = SessionPhase::TearingDown;
    }

    /// Start an operator-requested native revive and clear automatic restart
    /// accounting. The resume id remains intact for the new process.
    pub fn begin_manual_revive_at(&mut self, now: SystemTime) {
        self.restarts = 0;
        self.begin_restart_at_from(now, StatusSource::Manual);
    }

    /// Stop without automatically restarting. The row remains available for a
    /// future explicit revive operation.
    pub fn mark_resurrectable_at(&mut self, exit_code: Option<u32>, now: SystemTime) {
        self.mark_resurrectable_at_from(exit_code, now, StatusSource::ProcessExit);
    }

    pub fn mark_resurrectable_at_from(
        &mut self,
        exit_code: Option<u32>,
        now: SystemTime,
        source: StatusSource,
    ) {
        self.pid = None;
        if exit_code.is_some() {
            self.last_exit_code = exit_code;
        }
        self.backoff_until = None;
        self.set_status_at(SessionStatus::Exited, now, source, true);
    }

    /// Schedule one automatic restart using exponential backoff. The final
    /// failed attempt transitions to a terminal `Failed` state.
    pub fn schedule_restart_at(
        &mut self,
        exit_code: Option<u32>,
        now: SystemTime,
    ) -> RestartDecision {
        self.schedule_restart_at_from(exit_code, now, StatusSource::ProcessExit)
    }

    pub fn schedule_restart_at_from(
        &mut self,
        exit_code: Option<u32>,
        now: SystemTime,
        source: StatusSource,
    ) -> RestartDecision {
        self.pid = None;
        if exit_code.is_some() {
            self.last_exit_code = exit_code;
        }
        if self.restarts >= MAX_RESTARTS {
            self.backoff_until = None;
            self.set_status_at(SessionStatus::Exited, now, source, true);
            self.phase = SessionPhase::Failed;
            self.health = SessionHealth::Failed;
            return RestartDecision::Failed;
        }

        self.restarts += 1;
        let delay = restart_backoff(self.restarts);
        self.backoff_until = Some(now + delay);
        self.set_status_at(SessionStatus::Exited, now, source, true);
        self.phase = SessionPhase::Backoff;
        self.health = SessionHealth::Degraded;
        RestartDecision::Backoff(delay)
    }

    pub fn in_state_for(&self) -> Duration {
        SystemTime::now()
            .duration_since(self.state_since)
            .unwrap_or_default()
    }

    /// How long the session has held its current status — the number that tells
    /// you at a glance which agent is wedged.
    pub fn in_status_for(&self) -> Duration {
        SystemTime::now()
            .duration_since(self.status_since)
            .unwrap_or_default()
    }

    pub fn set_last_line(&mut self, line: &str) {
        self.last_line = trim_for_row(line, 160);
    }
}

/// Mint a per-session hook secret. Failure fails closed at hook delivery: an
/// empty token is never accepted by the registry, while the session can still
/// be supervised and the operator can see the provider's output.
pub(crate) fn fresh_hook_token() -> String {
    let mut bytes = [0u8; 32];
    if let Err(error) = getrandom::fill(&mut bytes) {
        tracing::error!(%error, "could not generate per-session hook token");
        return String::new();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Mint the UUID that Claude writes into its transcript filename when the
/// launcher supplies `--session-id`. Returning `None` keeps the launch usable
/// if the OS random source is unavailable; transcript discovery then uses the
/// documented heuristic fallback.
pub(crate) fn fresh_native_session_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    if let Err(error) = getrandom::fill(&mut bytes) {
        tracing::error!(%error, "could not generate provider session id");
        return None;
    }
    // RFC 4122 version 4 / variant 1 bits. The value is still accepted by the
    // narrower resume-id validator used at every process boundary.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

fn restart_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(63);
    let multiplier = 1u128 << exponent;
    let millis = RESTART_BACKOFF_BASE.as_millis().saturating_mul(multiplier);
    let capped = millis.min(RESTART_BACKOFF_MAX.as_millis());
    Duration::from_millis(capped as u64)
}

/// Sort key for the fleet list: attention first, then longest-waiting.
///
/// Compare the stored transition timestamps directly. Reading the clock inside
/// a comparator makes the result change during one sort and can violate the
/// total-order contract required by `sort_by`.
pub fn fleet_order(a: &Session, b: &Session) -> std::cmp::Ordering {
    b.status
        .cmp(&a.status)
        .then_with(|| b.status_since.cmp(&a.status_since))
        .then_with(|| a.id.cmp(&b.id))
}

fn folder_label(cwd: &std::path::Path) -> String {
    cwd.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
}

/// Collapse whitespace and clip — agent output is full of carriage returns and
/// spinner frames that would otherwise wreck the row layout.
fn trim_for_row(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(max.min(s.len()));
    let mut space = false;
    for ch in s.chars() {
        let ch = if ch.is_whitespace() || ch.is_control() {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if space || out.is_empty() {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        out.push(ch);
        if out.chars().count() >= max {
            break;
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::StatusReasonKind;
    use crate::launch::spec_for;
    use std::path::Path;

    fn session(status: SessionStatus) -> Session {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut s = Session::new(SessionId::new(1), &spec);
        s.status = status;
        s
    }

    #[test]
    fn fresh_native_session_ids_are_uuid_shaped_and_unique() {
        let first = fresh_native_session_id().expect("OS random source");
        let second = fresh_native_session_id().expect("OS random source");
        assert_eq!(first.len(), 36);
        assert_eq!(second.len(), 36);
        assert!(crate::launch::is_valid_resume_id(&first));
        assert!(crate::launch::is_valid_resume_id(&second));
        assert_ne!(first, second);
        assert_eq!(&first[14..15], "4");
        assert!(matches!(first.as_bytes()[19], b'8'..=b'b'));
    }

    #[test]
    fn attention_sorts_to_the_top() {
        let mut v = [
            session(SessionStatus::Idle),
            session(SessionStatus::NeedsYou),
            session(SessionStatus::Working),
        ];
        v.sort_by(fleet_order);
        assert_eq!(v[0].status, SessionStatus::NeedsYou);
        assert_eq!(v[2].status, SessionStatus::Idle);
    }

    #[test]
    fn fleet_order_is_stable_when_status_clocks_match() {
        let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1234);
        let mut first = session(SessionStatus::Working);
        first.id = SessionId::new(2);
        first.status_since = stamp;
        let mut second = session(SessionStatus::Working);
        second.id = SessionId::new(1);
        second.status_since = stamp;
        let mut sessions = [first, second];

        sessions.sort_by(fleet_order);
        let expected = sessions
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        for _ in 0..5 {
            sessions.sort_by(fleet_order);
            assert_eq!(
                sessions
                    .iter()
                    .map(|session| session.id.clone())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        assert_eq!(expected, [SessionId::new(1), SessionId::new(2)]);
    }

    #[test]
    fn needs_you_raises_unread_but_repeat_transitions_do_not_restamp() {
        let mut s = session(SessionStatus::Working);
        s.set_status(SessionStatus::NeedsYou);
        assert!(s.unread);
        let stamp = s.status_since;
        s.set_status(SessionStatus::NeedsYou);
        assert_eq!(
            s.status_since, stamp,
            "a repeated status must not reset the timer"
        );
    }

    #[test]
    fn spinner_noise_does_not_wreck_the_row() {
        let mut s = session(SessionStatus::Working);
        s.set_last_line("  Thinking\r\n\t  \u{1b}   about   it  ");
        assert_eq!(s.last_line, "Thinking about it");
    }

    #[test]
    fn row_text_is_clipped() {
        let mut s = session(SessionStatus::Idle);
        s.set_last_line(&"x".repeat(500));
        assert_eq!(s.last_line.chars().count(), 160);
    }

    #[test]
    fn optional_row_metadata_is_backward_compatible() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let session = Session::new(SessionId::new(6), &spec);
        let mut value = serde_json::to_value(&session).unwrap();
        value.as_object_mut().unwrap().remove("branch");
        value.as_object_mut().unwrap().remove("ports");
        value.as_object_mut().unwrap().remove("tool_progress");
        value.as_object_mut().unwrap().remove("status_history");
        value.as_object_mut().unwrap().remove("reviewed_digest");
        let restored: Session = serde_json::from_value(value).unwrap();
        assert_eq!(restored.branch, None);
        assert!(restored.ports.is_empty());
        assert_eq!(restored.tool_progress, None);
        assert!(restored.status_history.is_empty());
        assert_eq!(restored.reviewed_digest, None);
    }

    #[test]
    fn status_history_records_source_and_timestamp() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut session = Session::new(SessionId::new(8), &spec);
        let before = SystemTime::now();
        session.set_status_from(SessionStatus::NeedsYou, StatusSource::Hook);
        let transition = session.status_history.last().expect("status evidence");
        assert_eq!(transition.from, Some(SessionStatus::Starting));
        assert_eq!(transition.to, SessionStatus::NeedsYou);
        assert_eq!(transition.source, StatusSource::Hook);
        assert_eq!(transition.reason.kind, StatusReasonKind::AgentHook);
        assert_eq!(transition.reason.args["status"], "needs-you");
        let wire = serde_json::to_value(transition).expect("status transition serializes");
        assert_eq!(wire["reason"]["kind"], "agent-hook");
        assert_eq!(wire["reason"]["args"]["status"], "needs-you");
        assert!(transition.at >= before);
    }

    #[test]
    fn unnamed_sessions_borrow_the_folder_name() {
        let spec = spec_for(Agent::Codex, Path::new(r"C:\Users\me\repos\TerminalAI"));
        assert_eq!(Session::new(SessionId::new(2), &spec).name, "TerminalAI");
    }

    #[test]
    fn supervision_starts_explicitly_and_tracks_process_identity() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut session = Session::new(SessionId::new(3), &spec);
        assert_eq!(session.phase, SessionPhase::Starting);
        assert_eq!(session.health, SessionHealth::Starting);
        assert_eq!(session.restarts, 0);
        assert_eq!(session.pid, None);

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        session.mark_spawned_at(Some(42), now);
        assert_eq!(session.pid, Some(42));
        assert_eq!(session.health, SessionHealth::Healthy);
        assert_eq!(session.state_since, now);
    }

    #[test]
    fn restart_backoff_is_exponential_and_eventually_terminal() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut session = Session::new(SessionId::new(4), &spec);
        let mut now = SystemTime::UNIX_EPOCH;
        for expected in [250, 500, 1_000, 2_000, 4_000] {
            assert_eq!(
                session.schedule_restart_at(Some(17), now),
                RestartDecision::Backoff(Duration::from_millis(expected))
            );
            assert_eq!(session.phase, SessionPhase::Backoff);
            assert_eq!(session.health, SessionHealth::Degraded);
            assert_eq!(
                session.backoff_until,
                Some(now + Duration::from_millis(expected))
            );
            now += Duration::from_millis(expected);
        }
        assert_eq!(session.restarts, MAX_RESTARTS);
        assert_eq!(
            session.schedule_restart_at(Some(99), now),
            RestartDecision::Failed
        );
        assert_eq!(session.phase, SessionPhase::Failed);
        assert_eq!(session.health, SessionHealth::Failed);
        assert_eq!(session.last_exit_code, Some(99));
        assert_eq!(session.backoff_until, None);
    }

    #[test]
    fn manual_exit_is_resurrectable_without_restart() {
        let spec = spec_for(Agent::Codex, Path::new("."));
        let mut session = Session::new(SessionId::new(5), &spec);
        session.mark_spawned_at(Some(7), SystemTime::UNIX_EPOCH);
        session.mark_resurrectable_at(Some(3), SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        assert_eq!(session.phase, SessionPhase::Resurrectable);
        assert_eq!(session.health, SessionHealth::Degraded);
        assert_eq!(session.pid, None);
        assert_eq!(session.last_exit_code, Some(3));
        assert_eq!(session.restarts, 0);
    }
}
