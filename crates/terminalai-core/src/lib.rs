//! Core session model for TerminalAI.
//!
//! Three concerns live here, deliberately separated so the GUI layer stays thin:
//!
//! * [`agent`] — locating the real `claude` / `codex` executables on this machine.
//! * [`launch`] — turning a user's GUI choices into an exact argument vector.
//! * [`pty`] — spawning that argument vector on a ConPTY and pumping its output.
//!
//! Nothing in this crate depends on a UI toolkit; `terminalai-probe` drives it
//! headlessly and the Tauri shell drives it from a command handler.

pub mod agent;
pub mod hook_config;
pub mod hooks;
pub mod launch;
pub mod pty;
pub mod registry;
pub mod session;
pub mod transcript;

pub use agent::{Agent, AgentBinary, ResolveError};
pub use hook_config::{
    command_for as hook_command, config_path as hook_config_path, install_at as install_hooks_at,
    preview as hook_config_preview, remove_at as remove_hooks_at, status_at as hook_status_at,
    HookChange, HookConfigError, HookStatus, MANAGED_MARKER,
};
pub use hooks::{parse_hook, HookEvent, HookNotification, HookParseError, HookSignal};
pub use launch::{Effort, LaunchError, LaunchSpec, Permission, ResolvedCommand, Resume, Sandbox};
pub use pty::{PtySession, PtySize};
pub use registry::{RegistryError, RegistryEvent, SessionRegistry};
pub use session::{
    RestartDecision, Session, SessionHealth, SessionId, SessionPhase, SessionStatus, MAX_RESTARTS,
    RESTART_BACKOFF_BASE, RESTART_BACKOFF_MAX,
};
pub use transcript::{
    PricingTable, TokenRates, TranscriptAccumulator, TranscriptError, UsageTotals,
};
