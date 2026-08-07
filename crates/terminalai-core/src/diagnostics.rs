//! Structured status evidence retained with each session.
//!
//! Status is useful only when an operator can tell why the supervisor chose
//! it. The bounded history is part of the session model so it travels through
//! the daemon protocol and survives the versioned session store.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::session::SessionStatus;

pub const MAX_STATUS_HISTORY: usize = 64;
pub const MAX_LOG_ENTRIES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusSource {
    Launch,
    Admission,
    Hook,
    AppServer,
    Transcript,
    PtyOutput,
    ProcessStart,
    ProcessExit,
    ProcessQuery,
    Supervisor,
    Manual,
    Restore,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusReasonKind {
    SessionCreated,
    AdmissionQueued,
    AdmissionGranted,
    AgentHook,
    AppServerEvent,
    TranscriptEvent,
    PtyOutput,
    ProcessStarted,
    ProcessExited,
    ProcessQuery,
    Supervisor,
    Manual,
    Restored,
    StatusChanged,
    /// The agent has started compacting its context window.
    ///
    /// Recorded even when the status does not move, which is the usual case:
    /// an agent that compacts mid-turn is `Thinking` before and after, so the
    /// transition-only history showed nothing at all for a pause that can run
    /// to tens of seconds. That silence is indistinguishable from a stall.
    ContextCompacting,
    /// Compaction finished. The occupancy reading taken before it described a
    /// window that no longer exists, so it is dropped rather than carried.
    ContextCompacted,
    /// The agent moved to a different working directory, so the row's folder
    /// and branch were re-read.
    WorkingDirectoryChanged,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatusReason {
    pub kind: StatusReasonKind,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
}

impl Default for StatusReason {
    fn default() -> Self {
        Self {
            kind: StatusReasonKind::Unknown,
            args: BTreeMap::new(),
        }
    }
}

impl StatusReason {
    pub fn for_transition(
        from: Option<SessionStatus>,
        to: SessionStatus,
        source: StatusSource,
        exit_code: Option<u32>,
    ) -> Self {
        let kind = match (from, to, source) {
            (None, _, _) => StatusReasonKind::SessionCreated,
            (_, SessionStatus::Queued, StatusSource::Admission) => {
                StatusReasonKind::AdmissionQueued
            }
            (Some(SessionStatus::Queued), SessionStatus::Starting, StatusSource::Admission) => {
                StatusReasonKind::AdmissionGranted
            }
            (_, _, StatusSource::Hook) => StatusReasonKind::AgentHook,
            (_, _, StatusSource::AppServer) => StatusReasonKind::AppServerEvent,
            (_, _, StatusSource::Transcript) => StatusReasonKind::TranscriptEvent,
            (_, _, StatusSource::PtyOutput) => StatusReasonKind::PtyOutput,
            (_, _, StatusSource::ProcessStart) => StatusReasonKind::ProcessStarted,
            (_, SessionStatus::Exited, StatusSource::ProcessExit) => {
                StatusReasonKind::ProcessExited
            }
            (_, _, StatusSource::ProcessQuery) => StatusReasonKind::ProcessQuery,
            (_, _, StatusSource::Supervisor) => StatusReasonKind::Supervisor,
            (_, _, StatusSource::Manual) => StatusReasonKind::Manual,
            (_, _, StatusSource::Restore) => StatusReasonKind::Restored,
            _ => StatusReasonKind::StatusChanged,
        };
        let mut args = BTreeMap::new();
        args.insert("status".into(), status_key(to).into());
        if matches!(kind, StatusReasonKind::ProcessExited) {
            args.insert(
                "code".into(),
                exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            );
        }
        Self { kind, args }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatusDiagnostic {
    pub at: SystemTime,
    pub from: Option<SessionStatus>,
    pub to: SessionStatus,
    pub source: StatusSource,
    #[serde(default)]
    pub reason: StatusReason,
    /// Kept only so stores written before the structured reason can still be
    /// read. New diagnostics never emit English detail text.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn status_key(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Exited => "exited",
        SessionStatus::Queued => "queued",
        SessionStatus::Unknown => "unknown",
        SessionStatus::Starting => "starting",
        SessionStatus::Idle => "idle",
        SessionStatus::Thinking => "thinking",
        SessionStatus::Working => "working",
        SessionStatus::RateLimited => "rate-limited",
        SessionStatus::NeedsYou => "needs-you",
        SessionStatus::AwaitingInput => "awaiting-input",
        SessionStatus::NeedsApproval => "needs-approval",
    }
}

/// One structured daemon record delivered to the in-app log panel.
///
/// The daemon keeps only [`MAX_LOG_ENTRIES`] records in memory. The fields map
/// preserves session identity and other structured values without forcing the
/// UI protocol to grow every time a diagnostic field is added.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    pub at: SystemTime,
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}
