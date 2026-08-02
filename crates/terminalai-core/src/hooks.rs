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
