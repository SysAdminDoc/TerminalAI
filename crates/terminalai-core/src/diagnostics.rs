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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatusDiagnostic {
    pub at: SystemTime,
    pub from: Option<SessionStatus>,
    pub to: SessionStatus,
    pub source: StatusSource,
    #[serde(default)]
    pub detail: Option<String>,
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
