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

/// How long a process must run before its predecessors' restarts stop counting
/// against it.
///
/// Every mature supervisor scopes the budget to a window rather than to the
/// lifetime of the thing it supervises: OTP pairs `intensity` with `period`,
/// systemd pairs `StartLimitBurst` with `StartLimitIntervalSec`, and Kubernetes
/// resets the CrashLoopBackOff counter after ten minutes of successful running.
/// Ten minutes is Kubernetes' number, chosen for the same reason: it is long
/// enough that a genuine crash loop cannot hide inside it, and short enough that
/// a session which crashes once a day recovers every day. Without it, five
/// restarts spread over a week permanently kill a session that ran healthily in
/// between.
pub const RESTART_WINDOW: Duration = Duration::from_secs(10 * 60);

/// How long a session may sit in one working state before the fleet calls it
/// stalled rather than busy.
///
/// `Working` is the status this file's own comment calls "the long tail where
/// sessions get stuck", and the dwell timer that measures it was formatted and
/// never compared against anything. Fifteen minutes is long enough that an
/// ordinary build, test run or large edit finishes inside it, and short enough
/// that a wedged session is surfaced while the operator still remembers asking
/// for it.
pub const STALL_THRESHOLD: Duration = Duration::from_secs(15 * 60);

/// How long a live session may give no evidence of life at all before the
/// supervisor counts one missed deadline against it.
///
/// Deliberately shorter than [`STALL_THRESHOLD`], because it measures something
/// different and much stronger. A stall is "has held one status a long time",
/// which a session running a twenty-minute test suite does while printing to
/// its pty the whole way. This is "has produced nothing at all" — no pty byte,
/// no transcript append, no hook event. A working agent emits *something* far
/// more often than every five minutes, so silence this long is a real signal
/// rather than a slow moment.
pub const PROGRESS_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// Consecutive missed deadlines before the fleet calls a session unresponsive.
///
/// Kubernetes' `failureThreshold` default, adopted for the reason it exists: a
/// single missed probe is a slow moment, not a verdict, and acting on one is
/// how a supervisor turns a busy machine into a restart storm. Three misses is
/// fifteen minutes of complete silence from a process that should be talking.
pub const PROGRESS_FAILURE_THRESHOLD: u32 = 3;

/// `STATUS_CONTROL_C_EXIT` — what a Windows console process reports after the
/// operator pressed Ctrl-C in its pane. It is a deliberate stop by a person,
/// not a fault, so the supervisor treats it as one.
pub const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;

/// Whether an exit is something to recover from.
///
/// Every mature supervisor draws this line: OTP calls it the `transient`
/// restart type and systemd calls it `Restart=on-abnormal`, and both restart a
/// child only when it ended abnormally. Restarting an agent that finished its
/// work re-runs work nobody asked for and bills quota for it, up to
/// [`MAX_RESTARTS`] times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// The agent ended on purpose. There is nothing to recover.
    Finished,
    /// The agent died, or died in a way we could not read. Bring it back.
    Abnormal,
}

/// Classify one process exit.
///
/// An unreadable exit code is abnormal: the supervisor cannot prove the agent
/// meant to stop, and the cost of a spurious restart is lower than the cost of
/// silently abandoning a crashed session.
pub fn classify_exit(exit_code: Option<u32>) -> ExitClass {
    match exit_code {
        Some(0) | Some(STATUS_CONTROL_C_EXIT) => ExitClass::Finished,
        _ => ExitClass::Abnormal,
    }
}

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
    /// The agent exited on purpose and the supervisor will not bring it back.
    /// Distinct from `Failed`, which is the supervisor giving up.
    Finished,
}

/// Health of the process and its supervision boundary.
///
/// Every variant but [`SessionHealth::Unresponsive`] is a function of the
/// status and the PID — that is, of *whether the process exists*. Unresponsive
/// is the one verdict carrying independent evidence: the process is running and
/// has stopped saying anything. Keeping the two apart is the point. A session
/// that is busy thinking and one that has wedged look identical to anything
/// that only asks whether a PID is alive, and a supervisor that cannot tell
/// them apart is the documented cause of restart storms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionHealth {
    Queued,
    Starting,
    Healthy,
    Degraded,
    Failed,
    /// Alive, and silent past [`PROGRESS_FAILURE_THRESHOLD`] deadlines.
    ///
    /// Never a restart trigger. This is a report to the operator, not a verdict
    /// on the process: only a *proven-dead* process is restarted, because the
    /// alternative is killing an agent that was thinking hard about a large
    /// repository.
    Unresponsive,
    /// Ended by design. Not degraded — there is nothing wrong with it.
    Finished,
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
    /// The agent exited cleanly. No restart is scheduled and none ever will be
    /// without an explicit operator action.
    Finished,
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
    /// True once the session has held a working status past
    /// [`STALL_THRESHOLD`]. Computed by the supervisor, which has a clock, and
    /// stored here so [`fleet_order`] stays a pure comparator over stamped
    /// values — reading the clock inside a comparator makes the answer change
    /// during one sort and can violate the total order `sort_by` requires.
    #[serde(default)]
    pub stalled: bool,
    /// When this session last gave any evidence of being alive: a pty byte, a
    /// transcript append, or a hook event.
    ///
    /// Distinct from `status_since`, which only moves when the status *changes*
    /// — a session can hold `Working` for an hour while producing output the
    /// whole time. `None` until the first signal arrives; the supervisor treats
    /// the process start as the first deadline in that case.
    #[serde(default)]
    pub last_progress_at: Option<SystemTime>,
    /// Consecutive missed progress deadlines. Reset by any evidence of life, so
    /// this only climbs while the session is genuinely silent.
    #[serde(default)]
    pub missed_progress_deadlines: u32,
    pub restarts: u32,
    /// When the current process started, so the restart budget can be scoped to
    /// a window instead of counting for the life of the session. `None` before
    /// the first spawn and after every exit.
    #[serde(default)]
    pub process_started_at: Option<SystemTime>,
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
    /// Private commit sampled from the session's process, in bytes. `None` means
    /// it has not been sampled or could not be read — never that it is using
    /// nothing.
    #[serde(default)]
    pub memory_bytes: Option<u64>,
    /// The session's private commit reached the per-session job cap, so its
    /// allocations are being refused. Reported rather than left to look like an
    /// ordinary crash.
    #[serde(default)]
    pub memory_limited: bool,
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
    /// The most-consumed quota window the agent has reported, whether or not it
    /// is currently refusing work.
    ///
    /// Distinct from `rate_limit`, which means "the provider is saying no right
    /// now" and is cleared the moment it stops. This one is the headroom
    /// reading, and keeping it is what lets the fleet warn before a window
    /// closes instead of only reporting that it has.
    #[serde(default)]
    pub quota: Option<RateLimit>,
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
            stalled: false,
            last_progress_at: None,
            missed_progress_deadlines: 0,
            restarts: 0,
            process_started_at: None,
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
            memory_bytes: None,
            memory_limited: false,
            tokens: None,
            last_message: None,
            rate_limit: None,
            quota: None,
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
        // A status the agent itself reported is evidence it is alive, whatever
        // the status says. Supervisor-sourced transitions are not: the
        // supervisor deciding a session is `Unknown` says nothing about whether
        // the agent is still working, and counting it as progress would let the
        // fleet reassure itself.
        if matches!(
            source,
            StatusSource::Hook
                | StatusSource::AppServer
                | StatusSource::Transcript
                | StatusSource::PtyOutput
        ) {
            self.note_progress_at(now);
        }
        self.health = match status {
            SessionStatus::Queued => SessionHealth::Queued,
            SessionStatus::Starting => SessionHealth::Starting,
            SessionStatus::Exited => SessionHealth::Degraded,
            // A silence verdict outlives a status change: the agent moving from
            // Thinking to Working while saying nothing to us is the supervisor's
            // own bookkeeping, not the session speaking. Only real evidence
            // clears it, and `note_progress_at` above is the only thing that
            // does.
            _ if self.health == SessionHealth::Unresponsive => SessionHealth::Unresponsive,
            _ if self.pid.is_some() => SessionHealth::Healthy,
            _ => SessionHealth::Degraded,
        };
        if previous != status {
            self.record_status_transition(Some(previous), status, source, now);
        }
    }

    /// Record evidence that the session is alive.
    ///
    /// Called for every pty byte, transcript append and agent hook event. It is
    /// deliberately cheap and deliberately blind to *what* the evidence was:
    /// the supervisor's question is only whether the process is still talking.
    pub fn note_progress_at(&mut self, now: SystemTime) {
        self.last_progress_at = Some(now);
        self.missed_progress_deadlines = 0;
        if self.health == SessionHealth::Unresponsive {
            self.health = if self.pid.is_some() {
                SessionHealth::Healthy
            } else {
                SessionHealth::Degraded
            };
        }
    }

    /// The supervisor's periodic liveness check. Returns whether the verdict
    /// changed, so the caller only republishes a row that actually moved.
    ///
    /// This never restarts anything and never kills anything. It answers one
    /// question — has this session gone quiet — and the answer is a report.
    pub fn review_progress_at(&mut self, now: SystemTime) -> bool {
        // Only a session that should be talking. A queued session has no
        // process, an idle one is waiting on the operator by definition, and a
        // rate-limited one is silent for a reason it already told us.
        if !matches!(
            self.status,
            SessionStatus::Working | SessionStatus::Thinking | SessionStatus::Starting
        ) || self.pid.is_none()
        {
            return false;
        }
        // Before the first signal, the process start is the deadline's origin —
        // a session that has never said anything is still on the clock.
        let since = self
            .last_progress_at
            .or(self.process_started_at)
            .unwrap_or(self.status_since);
        let Ok(silent_for) = now.duration_since(since) else {
            return false;
        };
        // One deadline per elapsed period, so a supervisor that missed a sweep
        // does not under-count the silence it slept through.
        let missed = (silent_for.as_secs() / PROGRESS_DEADLINE.as_secs()) as u32;
        if missed == self.missed_progress_deadlines {
            return false;
        }
        self.missed_progress_deadlines = missed;
        let unresponsive = missed >= PROGRESS_FAILURE_THRESHOLD;
        if unresponsive && self.health != SessionHealth::Unresponsive {
            self.health = SessionHealth::Unresponsive;
            return true;
        }
        false
    }

    /// How long the session has been silent, when it has been silent at all.
    pub fn silent_for(&self, now: SystemTime) -> Option<Duration> {
        let since = self.last_progress_at.or(self.process_started_at)?;
        now.duration_since(since).ok()
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
        self.process_started_at = Some(now);
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
        // Classify before counting. An agent that finished its work is not
        // spending a restart from the budget, and it is not coming back on its
        // own — it is done, and the row says so.
        // A process that ran for a full window earned a clean slate. Scoping the
        // budget this way is what stops five restarts spread over a week from
        // permanently killing a session that ran healthily in between.
        if self
            .process_started_at
            .and_then(|started| now.duration_since(started).ok())
            .is_some_and(|ran_for| ran_for >= RESTART_WINDOW)
        {
            self.restarts = 0;
        }
        self.process_started_at = None;
        if classify_exit(exit_code) == ExitClass::Finished {
            self.backoff_until = None;
            self.set_status_at(SessionStatus::Exited, now, source, true);
            self.phase = SessionPhase::Finished;
            self.health = SessionHealth::Finished;
            return RestartDecision::Finished;
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

    /// Whether the session has held a working status past [`STALL_THRESHOLD`].
    ///
    /// Only the working states: an idle session is not stuck, it is done, and a
    /// session waiting on the operator is already at the top of the list for a
    /// better reason.
    pub fn is_stalled_at(&self, now: SystemTime) -> bool {
        if !matches!(
            self.status,
            SessionStatus::Working | SessionStatus::Thinking
        ) {
            return false;
        }
        now.duration_since(self.status_since)
            .is_ok_and(|held| held >= STALL_THRESHOLD)
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

/// Full jitter: `random(0, min(cap, base·2^n))`.
///
/// Failures here are correlated by construction — one provider rate limit or one
/// network drop takes every session in the fleet at the same instant — so a
/// deterministic delay guarantees all of them retry together against the service
/// that just refused them. AWS measured full jitter beating un-jittered backoff
/// by over 50% on contending calls, and it is the variant that spreads a
/// synchronised fleet fastest.
fn restart_backoff(attempt: u32) -> Duration {
    Duration::from_millis(jitter(restart_backoff_ceiling(attempt).as_millis() as u64))
}

/// The un-jittered ceiling the delay is drawn from. Separate so a test can pin
/// the exponential growth without depending on the random draw.
fn restart_backoff_ceiling(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(63);
    let multiplier = 1u128 << exponent;
    let millis = RESTART_BACKOFF_BASE.as_millis().saturating_mul(multiplier);
    let capped = millis.min(RESTART_BACKOFF_MAX.as_millis());
    Duration::from_millis(capped as u64)
}

/// Uniform draw from `0..=ceiling`.
///
/// `getrandom` is already a workspace dependency, so this costs no new crate. A
/// random source that fails returns the ceiling rather than zero: an immediate
/// unjittered retry into a provider that just refused the whole fleet is the one
/// outcome worth avoiding.
fn jitter(ceiling_millis: u64) -> u64 {
    if ceiling_millis == 0 {
        return 0;
    }
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return ceiling_millis;
    }
    u64::from_le_bytes(bytes) % (ceiling_millis + 1)
}

/// Sort key for the fleet list: attention first, then longest-waiting.
///
/// Compare the stored transition timestamps directly. Reading the clock inside
/// a comparator makes the result change during one sort and can violate the
/// total-order contract required by `sort_by`.
pub fn fleet_order(a: &Session, b: &Session) -> std::cmp::Ordering {
    b.status
        .cmp(&a.status)
        // A stalled session outranks a healthy one in the same status. Without
        // this the ordering within `Working` is newest-first, so the session
        // stuck longest sorted last — in precisely the status this file calls
        // the long tail where sessions get stuck.
        .then_with(|| b.stalled.cmp(&a.stalled))
        .then_with(|| {
            if a.stalled && b.stalled {
                // Among the stuck, longest first: that is the one to look at.
                a.status_since.cmp(&b.status_since)
            } else {
                b.status_since.cmp(&a.status_since)
            }
        })
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
        for ceiling in [250, 500, 1_000, 2_000, 4_000] {
            let RestartDecision::Backoff(delay) = session.schedule_restart_at(Some(17), now) else {
                panic!("attempt {} did not schedule a restart", session.restarts);
            };
            // Full jitter: the delay is drawn from zero up to the exponential
            // ceiling, so pin the ceiling rather than the draw.
            assert!(
                delay <= Duration::from_millis(ceiling),
                "delay {delay:?} exceeded its ceiling of {ceiling}ms"
            );
            assert_eq!(session.phase, SessionPhase::Backoff);
            assert_eq!(session.health, SessionHealth::Degraded);
            assert_eq!(session.backoff_until, Some(now + delay));
            now += Duration::from_millis(ceiling);
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

    #[test]
    fn the_restart_budget_resets_after_a_window_of_continuous_running() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut session = Session::new(SessionId::new(20), &spec);
        let mut now = SystemTime::UNIX_EPOCH;

        // Four crashes in quick succession spend four of the five.
        for _ in 0..4 {
            session.mark_spawned_at(Some(1), now);
            now += Duration::from_secs(5);
            assert!(matches!(
                session.schedule_restart_at(Some(1), now),
                RestartDecision::Backoff(_)
            ));
            now += Duration::from_secs(1);
        }
        assert_eq!(session.restarts, 4);

        // The fifth process runs out a full window before it dies. Without the
        // window that crash is the sixth strike and the session is dead forever;
        // with it, the budget starts over.
        session.mark_spawned_at(Some(2), now);
        now += RESTART_WINDOW + Duration::from_secs(1);
        assert!(matches!(
            session.schedule_restart_at(Some(1), now),
            RestartDecision::Backoff(_)
        ));
        assert_eq!(session.restarts, 1, "a healthy run did not clear the budget");
    }

    #[test]
    fn a_short_lived_process_does_not_clear_the_budget() {
        let spec = spec_for(Agent::Codex, Path::new("."));
        let mut session = Session::new(SessionId::new(21), &spec);
        let mut now = SystemTime::UNIX_EPOCH;
        for _ in 0..MAX_RESTARTS {
            session.mark_spawned_at(Some(1), now);
            now += RESTART_WINDOW - Duration::from_secs(1);
            assert!(matches!(
                session.schedule_restart_at(Some(1), now),
                RestartDecision::Backoff(_)
            ));
            now += Duration::from_secs(1);
        }
        session.mark_spawned_at(Some(1), now);
        now += Duration::from_secs(1);
        assert_eq!(
            session.schedule_restart_at(Some(1), now),
            RestartDecision::Failed,
            "a crash loop escaped the budget"
        );
    }

    /// One provider rate limit takes every session at once, so a deterministic
    /// backoff would have all of them retry at the same instants against the
    /// service that just refused them.
    #[test]
    fn a_fleet_failing_together_does_not_retry_in_lockstep() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let now = SystemTime::UNIX_EPOCH;
        let delays: std::collections::BTreeSet<_> = (0..24)
            .map(|index| {
                let mut session = Session::new(SessionId::new(index), &spec);
                // Third attempt: a 1s ceiling leaves ample room to distinguish.
                session.restarts = 2;
                match session.schedule_restart_at(Some(1), now) {
                    RestartDecision::Backoff(delay) => delay,
                    other => panic!("expected a backoff, got {other:?}"),
                }
            })
            .collect();
        assert!(
            delays.len() > 12,
            "24 sessions failing in the same instant produced only {} distinct delays",
            delays.len()
        );
        assert!(delays.iter().all(|delay| *delay <= Duration::from_secs(1)));
    }

    #[test]
    fn a_clean_exit_is_finished_rather_than_restarted() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut session = Session::new(SessionId::new(6), &spec);
        session.mark_spawned_at(Some(11), SystemTime::UNIX_EPOCH);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
        assert_eq!(
            session.schedule_restart_at(Some(0), now),
            RestartDecision::Finished
        );
        assert_eq!(session.status, SessionStatus::Exited);
        assert_eq!(session.phase, SessionPhase::Finished);
        assert_eq!(session.health, SessionHealth::Finished);
        assert_eq!(session.last_exit_code, Some(0));
        assert_eq!(session.backoff_until, None);
        // The budget is untouched: finishing is not a failure, so a later crash
        // still gets its full five attempts.
        assert_eq!(session.restarts, 0);
    }

    #[test]
    fn a_clean_exit_never_becomes_a_restart_no_matter_how_often_it_happens() {
        let spec = spec_for(Agent::Codex, Path::new("."));
        let mut session = Session::new(SessionId::new(7), &spec);
        let mut now = SystemTime::UNIX_EPOCH;
        for _ in 0..(MAX_RESTARTS + 3) {
            assert_eq!(
                session.schedule_restart_at(Some(0), now),
                RestartDecision::Finished
            );
            now += Duration::from_secs(1);
        }
        assert_eq!(session.restarts, 0);
        assert_eq!(session.phase, SessionPhase::Finished);
    }

    #[test]
    fn exit_classification_covers_ctrl_c_and_the_unreadable_case() {
        assert_eq!(classify_exit(Some(0)), ExitClass::Finished);
        // The operator pressed Ctrl-C in the pane. Deliberate, not a fault.
        assert_eq!(classify_exit(Some(STATUS_CONTROL_C_EXIT)), ExitClass::Finished);
        assert_eq!(classify_exit(Some(1)), ExitClass::Abnormal);
        assert_eq!(classify_exit(Some(0xC000_0005)), ExitClass::Abnormal);
        // Unknown is abnormal: a spurious restart costs less than silently
        // abandoning a session that crashed.
        assert_eq!(classify_exit(None), ExitClass::Abnormal);
    }

    #[test]
    fn an_abnormal_exit_still_restarts() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut session = Session::new(SessionId::new(8), &spec);
        let RestartDecision::Backoff(delay) =
            session.schedule_restart_at(Some(1), SystemTime::UNIX_EPOCH)
        else {
            panic!("an abnormal exit did not schedule a restart");
        };
        assert!(delay <= RESTART_BACKOFF_BASE);
        assert_eq!(session.restarts, 1);
        assert_eq!(session.phase, SessionPhase::Backoff);
    }

    /// The dwell timer was formatted and never compared against anything, and
    /// the ordering within `Working` was newest-first — so the session stuck
    /// longest sorted last, in the status this file calls the long tail.
    #[test]
    fn a_stalled_session_sorts_above_healthy_working_rows() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);

        let mut fresh = Session::new(SessionId::new(1), &spec);
        fresh.status = SessionStatus::Working;
        fresh.status_since = now - Duration::from_secs(30);

        let mut stuck = Session::new(SessionId::new(2), &spec);
        stuck.status = SessionStatus::Working;
        stuck.status_since = now - STALL_THRESHOLD - Duration::from_secs(60);

        let mut worse = Session::new(SessionId::new(3), &spec);
        worse.status = SessionStatus::Working;
        worse.status_since = now - STALL_THRESHOLD * 4;

        assert!(!fresh.is_stalled_at(now));
        assert!(stuck.is_stalled_at(now));
        assert!(worse.is_stalled_at(now));
        for session in [&mut fresh, &mut stuck, &mut worse] {
            session.stalled = session.is_stalled_at(now);
        }

        let mut rows = [fresh.clone(), stuck.clone(), worse.clone()];
        rows.sort_by(fleet_order);
        assert_eq!(
            rows.iter().map(|row| row.id.0.as_str()).collect::<Vec<_>>(),
            // Longest-stuck first, then the other stalled row, then the healthy
            // one — the exact reverse of what the old ordering produced.
            vec!["s0003", "s0002", "s0001"]
        );
    }

    /// A live session that should be talking, silent since `silent_for`.
    fn working_session(now: SystemTime, silent_for: Duration) -> Session {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut session = Session::new(SessionId::new(9), &spec);
        session.status = SessionStatus::Working;
        session.pid = Some(4242);
        session.health = SessionHealth::Healthy;
        session.process_started_at = Some(now - silent_for);
        session.last_progress_at = Some(now - silent_for);
        session
    }

    /// The distinction this whole verdict exists for. A twenty-minute test run
    /// that prints the whole way is *busy*, and the old health field — derived
    /// from status and PID alone — could not tell it from a wedge.
    #[test]
    fn a_session_producing_output_stays_healthy_however_long_it_works() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        let mut session = working_session(now, Duration::ZERO);
        // Four hours of work, reporting every minute.
        for minute in 1..=240u64 {
            session.note_progress_at(now + Duration::from_secs(minute * 60));
            assert!(
                !session.review_progress_at(now + Duration::from_secs(minute * 60)),
                "a session that just spoke was called unresponsive at minute {minute}"
            );
        }
        assert_eq!(session.health, SessionHealth::Healthy);
        assert_eq!(session.missed_progress_deadlines, 0);
    }

    #[test]
    fn a_silent_session_is_marked_unresponsive_and_is_not_restarted() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        let mut session = working_session(now, PROGRESS_DEADLINE * PROGRESS_FAILURE_THRESHOLD);

        assert!(session.review_progress_at(now), "the verdict did not change");
        assert_eq!(session.health, SessionHealth::Unresponsive);
        // The whole point: unresponsive is a report, not a restart. The process
        // is alive and may be thinking, so nothing kills it.
        assert_eq!(session.pid, Some(4242));
        assert_eq!(session.status, SessionStatus::Working);
        assert_eq!(session.restarts, 0);
        assert!(session.backoff_until.is_none());
        // And it does not re-announce itself on every later sweep.
        assert!(!session.review_progress_at(now + PROGRESS_DEADLINE));
    }

    #[test]
    fn one_missed_deadline_is_a_slow_moment_not_a_verdict() {
        // Acting on a single missed probe is how a supervisor turns a busy
        // machine into a restart storm.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        for missed in 1..PROGRESS_FAILURE_THRESHOLD {
            let mut session = working_session(now, PROGRESS_DEADLINE * missed);
            assert!(!session.review_progress_at(now), "{missed} misses acted on");
            assert_eq!(session.health, SessionHealth::Healthy);
            assert_eq!(session.missed_progress_deadlines, missed);
        }
    }

    #[test]
    fn evidence_of_life_clears_the_verdict() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        let mut session = working_session(now, PROGRESS_DEADLINE * PROGRESS_FAILURE_THRESHOLD);
        session.review_progress_at(now);
        assert_eq!(session.health, SessionHealth::Unresponsive);

        session.note_progress_at(now);
        assert_eq!(session.health, SessionHealth::Healthy);
        assert_eq!(session.missed_progress_deadlines, 0);
    }

    #[test]
    fn the_supervisors_own_bookkeeping_is_not_evidence_the_agent_is_alive() {
        // Otherwise the fleet reassures itself: the supervisor writes a status,
        // the status counts as progress, and a wedged session looks healthy
        // because something touched it.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        let mut session = working_session(now, PROGRESS_DEADLINE * PROGRESS_FAILURE_THRESHOLD);
        session.review_progress_at(now);
        assert_eq!(session.health, SessionHealth::Unresponsive);

        for source in [
            StatusSource::Supervisor,
            StatusSource::ProcessQuery,
            StatusSource::Manual,
        ] {
            let mut session = session.clone();
            session.set_status_at(SessionStatus::Thinking, now, source, true);
            assert_eq!(
                session.health,
                SessionHealth::Unresponsive,
                "{source:?} cleared a silence verdict it is no evidence against"
            );
        }

        // An agent-sourced transition is evidence, and does clear it.
        for source in [
            StatusSource::Hook,
            StatusSource::Transcript,
            StatusSource::PtyOutput,
            StatusSource::AppServer,
        ] {
            let mut session = session.clone();
            session.set_status_at(SessionStatus::Thinking, now, source, true);
            assert_eq!(
                session.health,
                SessionHealth::Healthy,
                "{source:?} is the agent speaking and should have cleared the verdict"
            );
        }
    }

    #[test]
    fn a_session_with_nothing_to_say_is_not_judged_for_saying_nothing() {
        // Idle means waiting on the operator; rate-limited already told us why
        // it is quiet; queued has no process at all.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        for status in [
            SessionStatus::Idle,
            SessionStatus::RateLimited,
            SessionStatus::NeedsApproval,
            SessionStatus::AwaitingInput,
            SessionStatus::Queued,
            SessionStatus::Exited,
        ] {
            let mut session = working_session(now, PROGRESS_DEADLINE * 10);
            session.status = status;
            assert!(
                !session.review_progress_at(now),
                "{status:?} was judged for being silent; it is not stuck, it is waiting"
            );
            assert_ne!(session.health, SessionHealth::Unresponsive, "{status:?}");
        }
    }

    #[test]
    fn a_session_with_no_process_is_not_judged_at_all() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        let mut session = working_session(now, PROGRESS_DEADLINE * 10);
        session.pid = None;
        assert!(!session.review_progress_at(now));
        assert_ne!(session.health, SessionHealth::Unresponsive);
    }

    #[test]
    fn a_missed_sweep_does_not_under_count_the_silence_it_slept_through() {
        // The supervisor is not guaranteed to run on time. Counting one miss
        // per sweep rather than per elapsed period would let a daemon that was
        // paused for an hour report a single missed deadline.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        let mut session = working_session(now, PROGRESS_DEADLINE * 12);
        assert!(session.review_progress_at(now));
        assert_eq!(session.missed_progress_deadlines, 12);
        assert_eq!(session.health, SessionHealth::Unresponsive);
    }

    #[test]
    fn silence_and_stall_are_different_questions() {
        // A session can be stalled without being silent — that is the case the
        // old model could not express, and the reason health had to stop being
        // a function of status and PID.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500_000);
        let mut session = working_session(now, Duration::ZERO);
        session.status_since = now - STALL_THRESHOLD - Duration::from_secs(60);

        assert!(session.is_stalled_at(now), "it has held Working a long time");
        assert!(!session.review_progress_at(now), "but it is still talking");
        assert_eq!(session.health, SessionHealth::Healthy);
    }

    #[test]
    fn only_a_working_session_can_stall() {
        let spec = spec_for(Agent::Codex, Path::new("."));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        for status in [
            SessionStatus::Idle,
            SessionStatus::NeedsYou,
            SessionStatus::Exited,
            SessionStatus::RateLimited,
        ] {
            let mut session = Session::new(SessionId::new(4), &spec);
            session.status = status;
            session.status_since = now - STALL_THRESHOLD * 10;
            assert!(
                !session.is_stalled_at(now),
                "{status:?} was called stalled; it is not stuck, it is waiting"
            );
        }
    }

    /// Ordering must stay a total order: `sort_by` may compare any two rows, and
    /// an inconsistent comparator can panic or silently scramble the list.
    #[test]
    fn the_stall_aware_order_is_still_a_total_order() {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        let mut rows = Vec::new();
        for (index, (status, stalled, offset)) in [
            (SessionStatus::Working, true, 5),
            (SessionStatus::Working, false, 5),
            (SessionStatus::Working, true, 900),
            (SessionStatus::NeedsYou, false, 1),
            (SessionStatus::Idle, false, 400),
            (SessionStatus::Thinking, true, 60),
        ]
        .into_iter()
        .enumerate()
        {
            let mut session = Session::new(SessionId::new(index as u64), &spec);
            session.status = status;
            session.stalled = stalled;
            session.status_since = base - Duration::from_secs(offset);
            rows.push(session);
        }
        for a in &rows {
            for b in &rows {
                assert_eq!(
                    fleet_order(a, b),
                    fleet_order(b, a).reverse(),
                    "{} vs {} is not antisymmetric",
                    a.id,
                    b.id
                );
            }
        }
        rows.sort_by(fleet_order);
        assert_eq!(rows.len(), 6);
    }
}
