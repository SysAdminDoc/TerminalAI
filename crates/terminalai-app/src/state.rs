//! Shared application state and command response payloads.
//!
//! The Tauri entry module is the composition root. It should not also be the
//! schema for every response or the storage registry for every feature; keeping
//! those types here gives command, preflight, event and lifecycle modules one
//! explicit boundary to depend on.

use std::sync::Mutex;

use serde::Serialize;
use terminalai_core::{AdmissionSnapshot, ReviewItem, Session, SessionId};

use super::{output::OutputChannels, preset::PresetStore, projects, work, workingset};

pub(crate) struct AppState {
    pub(crate) client: Mutex<Option<terminalai_daemon::DaemonClient>>,
    pub(crate) presets: PresetStore,
    pub(crate) project_roots: projects::ProjectRoots,
    pub(crate) prompts: work::PromptLibrary,
    pub(crate) work_run_store: work::WorkRunStore,
    pub(crate) work_schedule_store: work::WorkScheduleStore,
    pub(crate) working_sets: workingset::WorkingSetStore,
    pub(crate) output_channels: OutputChannels,
}

#[derive(Debug, Serialize)]
pub(crate) struct FleetSnapshot {
    pub(crate) sessions: Vec<Session>,
    pub(crate) focused: Option<SessionId>,
    pub(crate) admission: AdmissionSnapshot,
    pub(crate) store_quarantine: Option<String>,
    pub(crate) store_write_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReviewSnapshot {
    pub(crate) entries: Vec<ReviewItem>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LaunchReceipt {
    pub(crate) id: SessionId,
    pub(crate) queued: bool,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct PreflightCheck {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) state: String,
    pub(crate) detected: String,
    pub(crate) detail: Option<String>,
    pub(crate) can_fix: bool,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct PreflightReport {
    pub(crate) checks: Vec<PreflightCheck>,
}

#[derive(Serialize)]
pub(crate) struct LandResult {
    #[serde(flatten)]
    pub(crate) outcome: terminalai_core::land::LandOutcome,
    /// Present only when the request asked to archive.
    pub(crate) archive: Option<terminalai_daemon::ArchiveAfterLanding>,
}

pub(crate) const APP_USER_MODEL_ID: &str = "com.sysadmindoc.terminalai";
pub(crate) const PREFLIGHT_DAEMON_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(500);
