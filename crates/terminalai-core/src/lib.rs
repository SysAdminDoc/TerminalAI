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
pub mod launch;
pub mod pty;
pub mod registry;
pub mod session;

pub use agent::{Agent, AgentBinary, ResolveError};
pub use launch::{Effort, LaunchError, LaunchSpec, Permission, ResolvedCommand, Resume, Sandbox};
pub use pty::{PtySession, PtySize};
pub use registry::{RegistryError, RegistryEvent, SessionRegistry};
pub use session::{Session, SessionId, SessionStatus};
