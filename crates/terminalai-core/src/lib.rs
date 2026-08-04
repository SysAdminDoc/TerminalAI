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
pub mod app_server;
pub mod atomic_file;
pub mod capabilities;
pub mod diagnostics;
pub mod domain;
pub mod environment;
pub mod external;
pub mod grid;
pub mod hook_config;
pub mod hooks;
pub mod i18n;
pub mod land;
pub mod launch;
pub mod lease;
pub mod mcp;
pub mod notification;
pub(crate) mod process_tree;
pub mod project;
pub mod pty;
pub mod queue;
pub mod registry;
pub mod review;
pub mod roadmap;
pub mod scrollback;
pub mod session;
pub mod tail;
pub mod store;
pub mod template;
pub mod transcript;
pub mod work_queue;
pub mod worktree;

pub use agent::{Agent, AgentBinary, ResolveError};
pub use app_server::{
    parse_message as parse_app_server_message, AgentEvent, AppServerApprovalKind, AppServerEvent,
    AppServerMessage, AppServerNotification, AppServerParseError, AppServerRequest,
    AppServerResponse, AppServerThreadStatus, AppServerTokenUsage, RpcId,
};
pub use capabilities::{
    probe as probe_capabilities, AgentCapabilities, CapabilityError, ModelCapability,
};
pub use diagnostics::{
    LogEntry, StatusDiagnostic, StatusReason, StatusReasonKind, StatusSource, MAX_LOG_ENTRIES,
    MAX_STATUS_HISTORY,
};
pub use domain::{AgentDomain, AgentSession, DomainError, LocalPtyDomain, OutputHandler};
pub use environment::{
    EnvironmentError, EnvironmentSpec, DEFAULT_PORT_BASE, DEFAULT_PORT_COUNT, MAX_PORT_COUNT,
    PORT_BLOCK_STRIDE,
};
pub use i18n::{default_catalog, Catalog, CatalogError, DEFAULT_LOCALE};
pub use external::{claude_sessions, ExternalSession, ExternalState};
pub use grid::{TerminalGrid, TerminalGridSnapshot, DEFAULT_GRID_COLS, DEFAULT_GRID_ROWS};
pub use hook_config::{
    command_for as hook_command, config_path as hook_config_path, install_at as install_hooks_at,
    install_at_with_transport as install_hooks_at_with_transport,
    downgrade_claude_http_at as downgrade_claude_http_hooks_at,
    preview as hook_config_preview, remove_at as remove_hooks_at,
    status_at as hook_status_at, status_at_with_transport as hook_status_at_with_transport,
    HookChange, HookConfigError, HookStatus, HookTransport, MANAGED_MARKER,
};
pub use hooks::{
    parse_hook, parse_hook_in, HookEvent, HookNotification, HookParseError, HookSignal,
};
pub use launch::{Effort, LaunchError, LaunchSpec, Permission, ResolvedCommand, Resume, Sandbox};
pub use notification::{
    AttentionNotification, NotificationCenter, NotificationChange, NotificationEvent,
    SuppressionReason, LONG_TOOL_GRACE_PERIOD, STARTUP_GRACE_PERIOD,
};
pub use pty::{PtySession, PtySize};
pub use registry::{
    AdmissionConfig, AdmissionSnapshot, BroadcastRefusal, BroadcastResult, RegistryError,
    RegistryEvent, SessionRegistry,
    DEFAULT_MAX_LIVE_SESSIONS, DEFAULT_SESSION_BUDGET_USD,
};
pub use review::{collect_review, ReviewItem, MAX_REVIEW_DIFF_BYTES, REVIEW_REPOSITORY_TIMEOUT};
pub use session::{
    RestartDecision, Session, SessionHealth, SessionId, SessionPhase, SessionStatus, ToolProgress,
    MAX_RESTARTS, RESTART_BACKOFF_BASE, RESTART_BACKOFF_MAX,
};
pub use store::{
    ArchivedSession, SessionStoreError, SessionStoreSnapshot, StoredSession,
    SESSION_STORE_MAGIC, SESSION_STORE_SCHEMA_VERSION,
};
pub use transcript::{
    PricingTable, TokenRates, TranscriptAccumulator, TranscriptError, UsageTotals,
};
