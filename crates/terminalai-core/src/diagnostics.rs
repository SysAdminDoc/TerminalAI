//! Structured status evidence retained with each session.
//!
//! Status is useful only when an operator can tell why the supervisor chose
//! it. The bounded history is part of the session model so it travels through
//! the daemon protocol and survives the versioned session store.

use std::time::SystemTime;

use crate::session::SessionStatus;

pub const MAX_STATUS_HISTORY: usize = 64;

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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatusDiagnostic {
    pub at: SystemTime,
    pub from: Option<SessionStatus>,
    pub to: SessionStatus,
    pub source: StatusSource,
    #[serde(default)]
    pub detail: Option<String>,
}
