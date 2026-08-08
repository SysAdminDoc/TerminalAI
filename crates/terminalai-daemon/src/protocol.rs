use std::io::{self, BufRead};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use terminalai_core::agent::Agent;
use terminalai_core::launch::LaunchSpec;
use terminalai_core::{
    AdmissionSnapshot, AgentEvent, HookDeliveryStatus, HookEvent, RegistryEvent, ReviewItem,
    Session, SessionId,
};

use super::http_hooks::HookEndpoint;

pub const PROTOCOL_VERSION: u16 = 4;
/// Stable control-plane name. Protocol compatibility is negotiated in the
/// first frame, so changing the socket name would strand an older daemon that
/// still owns live sessions before the newer client can report the skew.
pub const PIPE_NAME: &str = "terminalai.control";
pub(super) const LEGACY_PIPE_NAME: &str = "terminalai.control.v2";
pub(super) const OUTGOING_QUEUE_CAPACITY: usize = 256;
/// Largest control frame either end will read.
///
/// A peer that sends bytes without a newline would otherwise grow a `String`
/// until the process is out of memory — and this is a local control plane, so
/// the peer is a program, not a person. The cap is generously above the largest
/// legitimate frame: a `ScrollbackHistory` response carries a bounded history
/// as a JSON array of numbers, which is several times larger on the wire than
/// its raw bytes.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Largest `Write` payload accepted for one session. Rejected, never truncated:
/// half a prompt reaching an agent is worse than none.
pub const MAX_WRITE_BYTES: usize = 256 * 1024;
pub(super) const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Most history one request may return: the in-memory ring plus one older
/// window, so the UI can replay bytes from before the pane's existing tail.
///
/// A response is one frame, and a frame is capped at [`MAX_FRAME_BYTES`]. The
/// bytes are JSON-encoded as an array of numbers, so the frame ceiling remains
/// several times larger than the raw history budget.
pub const MAX_HISTORY_BYTES: u64 =
    terminalai_core::registry::MAX_SCROLLBACK_BYTES as u64 + 128 * 1024;
/// Most sessions one broadcast may target.
///
/// Well above the fleet's ~30-session design point, and bounded so a malformed
/// client cannot make the daemon iterate an arbitrary list while holding the
/// registry lock once per entry.
pub const MAX_BROADCAST_TARGETS: usize = 256;

/// Read one newline-delimited frame, refusing anything over [`MAX_FRAME_BYTES`].
///
/// `BufRead::read_line` has no upper bound, so a peer that never sends a newline
/// can exhaust memory on either side of this protocol.
pub(super) fn read_frame<R: BufRead>(reader: &mut R, line: &mut String) -> Result<usize, IpcError> {
    line.clear();
    // One byte over the limit is enough to prove the frame is too long without
    // buffering the rest of it.
    let mut limited = std::io::Read::take(&mut *reader, MAX_FRAME_BYTES as u64 + 1);
    let read = limited.read_line(line)?;
    if read > MAX_FRAME_BYTES {
        return Err(IpcError::InvalidMessage(format!(
            "control frame exceeded {MAX_FRAME_BYTES} bytes"
        )));
    }
    Ok(read)
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("local control transport failed: {0}")]
    Io(#[from] io::Error),
    #[error("control protocol encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("control request timed out")]
    Timeout,
    #[error("control transport lock is poisoned: {0}")]
    Poisoned(&'static str),
    #[error("daemon rejected the request: {0}")]
    Remote(String),
    #[error(
        "incompatible control protocol: running daemon PID {daemon_pid} speaks v{daemon}, \
         client speaks v{client}; stop it with `Stop-Process -Id {daemon_pid}` on Windows, \
         then retry"
    )]
    VersionMismatch {
        daemon: u16,
        client: u16,
        daemon_pid: u32,
    },
    /// Diagnostic only: the pipe DACL authorizes the peer; the declared PID is
    /// not an authorization mechanism and may be self-reported inaccurately.
    #[error(
        "control peer PID diagnostic mismatch: client declared {expected}, transport reported \
         {actual:?}; the pipe DACL remains the authorization boundary"
    )]
    PeerMismatch { expected: u32, actual: Option<u32> },
    #[error("invalid control message: {0}")]
    InvalidMessage(String),
    #[error("session store could not be loaded: {0}")]
    Store(String),
    #[error("invalid admission configuration: {0}")]
    Configuration(String),
}

impl IpcError {
    pub(super) fn is_version_mismatch(&self) -> bool {
        matches!(self, Self::VersionMismatch { .. })
            || matches!(self, Self::Remote(message) if message
                .starts_with("incompatible control protocol:"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u16,
        client_pid: u32,
    },
    Subscribe,
    Ping,
    Close,
    /// Ask the daemon to tear down owned sessions and leave its accept loop.
    Shutdown,
    Snapshot,
    ReviewSnapshot,
    /// Sessions running outside this supervisor, read from the agent's own
    /// per-PID registry. Read-only: the daemon owns none of them.
    ExternalSessions,
    MarkReviewed {
        id: SessionId,
    },
    /// Checkouts under the worktree root that no live session owns.
    StaleWorktrees,
    /// Remove one surveyed checkout. Refused unless the branch is fully merged;
    /// the core enforces that, not only the window.
    ReapWorktree {
        stale: Box<terminalai_core::worktree::StaleWorktree>,
    },
    /// Sessions this supervisor finished and archived. Read-only history: the
    /// records carry no handle, only enough to relaunch the same command.
    SessionHistory,
    /// Read the daemon-wide admission policy.
    AdmissionConfig,
    /// Replace it without restarting. Running sessions are untouched.
    SetAdmission {
        max_live_sessions: usize,
        default_budget_usd: Option<f64>,
        spend_ceiling_usd: Option<f64>,
        spend_window_hours: Option<f64>,
        memory_budget_mb: Option<u64>,
        session_memory_cap_mb: Option<u64>,
        max_processes_per_session: Option<u32>,
    },
    /// Land a session's uncommitted work into a target repository, or refuse.
    /// Serialised daemon-side so two landings cannot interleave their
    /// precondition checks — the failure a hand-built merge queue works around.
    Land {
        request: Box<terminalai_core::land::LandRequest>,
    },
    Status {
        id: SessionId,
    },
    Resolve {
        agent: Agent,
        configured_path: Option<PathBuf>,
    },
    Capabilities {
        agent: Agent,
        configured_path: Option<PathBuf>,
    },
    Preview {
        spec: Box<LaunchSpec>,
        configured_path: Option<PathBuf>,
    },
    HookEndpoint,
    /// Read-only daemon-lifetime evidence that valid hooks reached the core.
    HookStatus,
    Hook {
        event: HookEvent,
        /// Secret minted for the supervised session and supplied by its hook
        /// adapter. The daemon-wide HTTP bearer is a separate transport gate.
        #[serde(default)]
        hook_token: Option<String>,
    },
    AgentEvent {
        event: AgentEvent,
    },
    Launch {
        spec: Box<LaunchSpec>,
        configured_path: Option<PathBuf>,
    },
    /// Raw bytes from the user-facing terminal. The registry derives the
    /// focused-pane edit guard from this stream; queue and broadcast writes do
    /// not come through this request.
    Write {
        id: SessionId,
        data: String,
    },
    Resize {
        id: SessionId,
        rows: u16,
        cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    Kill {
        id: SessionId,
    },
    Focus {
        id: Option<SessionId>,
    },
    MarkRead {
        id: SessionId,
    },
    TogglePin {
        id: SessionId,
    },
    /// Prompts waiting their turn on one session.
    QueuedPrompts {
        id: SessionId,
    },
    /// Add a prompt to a session's queue.
    EnqueuePrompt {
        id: SessionId,
        text: String,
    },
    EditQueuedPrompt {
        id: SessionId,
        prompt: u64,
        text: String,
    },
    RemoveQueuedPrompt {
        id: SessionId,
        prompt: u64,
    },
    ReorderQueuedPrompt {
        id: SessionId,
        prompt: u64,
        to: usize,
    },
    PauseQueue {
        id: SessionId,
    },
    ResumeQueue {
        id: SessionId,
    },
    /// Send the same bytes to several sessions at once.
    ///
    /// Answered with one result per session rather than a single status: a
    /// broadcast that says only "ok" or "failed" leaves the operator unable to
    /// tell which agents received the prompt, and re-sending to find out
    /// delivers it twice to the ones that already had it.
    Broadcast {
        ids: Vec<SessionId>,
        data: String,
    },
    Scrollback {
        id: SessionId,
    },
    /// History from the disk tier, reaching past the in-memory ring.
    ///
    /// `max_bytes` is clamped rather than rejected: a client asking for more
    /// than one frame can carry gets what fits, not an error it cannot act on.
    ScrollbackHistory {
        id: SessionId,
        max_bytes: u64,
    },
    /// The launch spec behind every live row, so a client can save a layout it
    /// can start again.
    ///
    /// The specs are the daemon's — a `Session` deliberately does not carry the
    /// spec that produced it, because the row is sent on every status change
    /// and the spec is large and unchanging. This is the read that fills that
    /// gap, asked for once when a layout is captured rather than continuously.
    FleetSpecs,
    /// Find a string in every session's retained output.
    ///
    /// A read, so it needs no write token. `max_bytes` is per session and
    /// clamped like `ScrollbackHistory`: the whole point is to search more than
    /// one pane, so an unclamped value would multiply the fleet's disk reads by
    /// whatever a client asked for.
    SearchScrollback {
        query: terminalai_core::search::SearchQuery,
        max_bytes: u64,
    },
    /// The parsed terminal state for a pinned or background session.
    ///
    /// Distinct from `Scrollback`, which returns raw bytes for the one focused
    /// renderer. A pinned pane needs a rendered grid it can show without an
    /// xterm instance of its own — the whole reason ~29 rows fit on a screen.
    GridSnapshot {
        id: SessionId,
    },
    Reattach {
        id: SessionId,
    },
    Revive {
        id: SessionId,
    },
    Archive {
        id: SessionId,
    },
}

/// What became of the session an opt-in landing asked to archive.
///
/// Reported rather than swallowed: archiving is refused for reasons the
/// operator can act on -- the session is still running, or its worktree still
/// holds unmerged commits -- and a landing that quietly did not archive looks
/// exactly like one that did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "archive", rename_all = "kebab-case")]
pub enum ArchiveAfterLanding {
    Archived,
    /// The landing succeeded; the archive did not. The work is landed either
    /// way, which is why this is not a landing failure.
    Refused {
        detail: String,
    },
}

/// One live row's launch spec, for saving a layout.
///
/// Boxed because `LaunchSpec` is by far the largest thing in this protocol and
/// a fleet of thirty would otherwise make this response variant dominate the
/// size of the whole enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSpec {
    pub id: SessionId,
    pub pinned: bool,
    pub spec: Box<terminalai_core::LaunchSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// Snapshot/status responses intentionally carry the full fleet row model;
// boxing them would add needless wire/API indirection without changing the
// local control-plane contract.
#[allow(clippy::large_enum_variant)]
pub enum Response {
    Hello {
        protocol: u16,
        daemon_pid: u32,
    },
    Admission {
        admission: AdmissionSettings,
    },
    Ok,
    Pong,
    Snapshot {
        sessions: Vec<Session>,
        focused: Option<SessionId>,
        admission: AdmissionSnapshot,
        #[serde(default)]
        store_quarantine: Option<String>,
        /// Why the fleet's state is not reaching disk, when it is not. `None`
        /// is the normal case. Reported on every snapshot rather than pushed
        /// once, so a failure that starts mid-session is still seen and one
        /// that recovers clears itself.
        #[serde(default)]
        store_write_error: Option<String>,
    },
    Land {
        outcome: terminalai_core::land::LandOutcome,
        /// What happened to the session afterwards, when the request asked for
        /// it to be archived. `None` means it did not ask.
        #[serde(default)]
        archive: Option<ArchiveAfterLanding>,
    },
    ReviewSnapshot {
        entries: Vec<ReviewItem>,
    },
    ExternalSessions {
        sessions: Vec<terminalai_core::ExternalSession>,
    },
    SessionHistory {
        archives: Vec<terminalai_core::ArchivedSession>,
    },
    StaleWorktrees {
        worktrees: Vec<terminalai_core::worktree::StaleWorktree>,
    },
    Status {
        session: Session,
        admission: AdmissionSnapshot,
    },
    Resolved {
        agent: Agent,
        path: PathBuf,
        origin: String,
    },
    Capabilities {
        capabilities: terminalai_core::AgentCapabilities,
    },
    Preview {
        command: String,
    },
    HookEndpoint {
        endpoint: HookEndpoint,
    },
    HookStatus {
        status: HookDeliveryStatus,
    },
    Hook {
        matched: bool,
        /// Where a checkout the agent is about to create should go.
        ///
        /// Only ever set for `WorktreeCreate`, and only when a worktree root is
        /// configured and the session is known. `None` means the agent keeps
        /// its own default — declining to place a checkout is safe, whereas
        /// naming a path this tool cannot then manage is not.
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        worktree_path: Option<std::path::PathBuf>,
    },
    AgentEvent {
        matched: bool,
    },
    Launched {
        id: SessionId,
        queued: bool,
    },
    Scrollback {
        data: Vec<u8>,
    },
    ScrollbackHistory {
        data: Vec<u8>,
    },
    FleetSpecs {
        specs: Vec<FleetSpec>,
    },
    SearchResults {
        /// Only sessions with at least one match, in fleet order.
        matches: Vec<terminalai_core::search::SessionMatches>,
        /// Bytes of history searched per session, after clamping — so a result
        /// says what it looked at rather than implying it read everything.
        searched_bytes: u64,
    },
    Broadcast {
        results: Vec<terminalai_core::BroadcastResult>,
    },
    QueuedPrompts {
        prompts: Vec<terminalai_core::queue::QueuedPrompt>,
    },
    Enqueued {
        prompt: u64,
    },
    GridSnapshot {
        grid: terminalai_core::TerminalGridSnapshot,
    },
    Reattached {
        data: Vec<u8>,
    },
    Revived {
        id: SessionId,
    },
    Archived {
        id: SessionId,
    },
    PinChanged {
        pinned: bool,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum WireMessage {
    Request { id: u64, request: Request },
    // Boxed: a `Response` carrying an `AdmissionSnapshot` dwarfs a `Request`,
    // and every slot in the bounded outgoing queue is sized for the largest
    // variant. `Box` is transparent to serde, so the wire format is unchanged.
    Response { id: u64, response: Box<Response> },
    Event { event: RegistryEvent },
}

/// The daemon-wide policy in the units the settings dialog edits.
///
/// Megabytes and hours rather than bytes and durations: the dialog is where
/// an operator types a number, and converting in one place keeps the two ends
/// from disagreeing about which unit a field is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmissionSettings {
    pub max_live_sessions: usize,
    pub default_budget_usd: Option<f64>,
    pub spend_ceiling_usd: Option<f64>,
    pub spend_window_hours: f64,
    pub memory_budget_mb: Option<u64>,
    pub session_memory_cap_mb: Option<u64>,
    pub max_processes_per_session: Option<u32>,
    /// Environment variables that were set when the daemon started. The dialog
    /// says so, because a value an operator did not type here came from
    /// somewhere and silently overriding it would be the confusing half of
    /// having two sources.
    pub from_environment: Vec<String>,
}

/// The environment variables that seed the admission policy at boot.
const ADMISSION_ENVIRONMENT: &[&str] = &[
    "TERMINALAI_MAX_LIVE_SESSIONS",
    "TERMINALAI_DEFAULT_BUDGET_USD",
    "TERMINALAI_SPEND_CEILING_USD",
    "TERMINALAI_SPEND_WINDOW_HOURS",
    "TERMINALAI_MEMORY_BUDGET_MB",
    "TERMINALAI_SESSION_MEMORY_CAP_MB",
    "TERMINALAI_MAX_PROCESSES_PER_SESSION",
];

pub(super) fn admission_settings(
    config: &terminalai_core::registry::AdmissionConfig,
) -> AdmissionSettings {
    AdmissionSettings {
        max_live_sessions: config.max_live_sessions,
        default_budget_usd: config.default_budget_usd,
        spend_ceiling_usd: config.spend_ceiling_usd,
        spend_window_hours: config.spend_window.as_secs_f64() / 3600.0,
        memory_budget_mb: config
            .memory_budget_bytes
            .map(|bytes| bytes / (1024 * 1024)),
        session_memory_cap_mb: config
            .session_memory_cap_bytes
            .map(|bytes| bytes / (1024 * 1024)),
        max_processes_per_session: config.max_processes_per_session,
        from_environment: ADMISSION_ENVIRONMENT
            .iter()
            .filter(|name| std::env::var_os(name).is_some())
            .map(|name| (*name).to_owned())
            .collect(),
    }
}
