//! Execution-domain seams for local and future non-local agent sessions.
//!
//! The registry supervises this small session contract instead of reaching
//! into a process handle. The default implementation is local ConPTY; a
//! remote domain can provide the same stream, input, resize and lifecycle
//! operations without pretending to own a Windows process.

use std::sync::Arc;

use crate::launch::ResolvedCommand;
use crate::pty::{PtyError, PtySession, PtySize, StopOutcome};

pub type OutputHandler = Box<dyn FnMut(&[u8]) + Send + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error(transparent)]
    LocalPty(#[from] PtyError),
    #[error("agent domain operation failed: {0}")]
    Message(String),
}

/// A running agent exposed by an [`AgentDomain`].
pub trait AgentSession: Send + Sync {
    fn write(&self, bytes: &[u8]) -> Result<(), DomainError>;
    fn resize(&self, size: PtySize) -> Result<(), DomainError>;
    fn pid(&self) -> Option<u32>;
    fn try_wait(&self) -> Result<Option<u32>, DomainError>;
    fn wait_for_exit(&self) -> Result<u32, DomainError>;
    fn kill(&self) -> Result<(), DomainError>;
    /// Stop the agent, giving it a chance to shut itself down first. The
    /// default is the hard kill, so a domain that has no graceful path is
    /// honest about it rather than silently claiming one.
    fn stop(&self) -> Result<StopOutcome, DomainError> {
        self.kill().map(|()| StopOutcome::Terminated)
    }
    /// Apply or restore the platform's background execution policy. Remote
    /// domains may leave the default no-op in place when they do not own a
    /// local process.
    fn set_background(&self, _background: bool) -> Result<(), DomainError> {
        Ok(())
    }
    /// Tell a local session that the focused terminal renderer is consuming
    /// its output. Remote domains may leave this as a no-op.
    fn set_renderer_attached(&self, _attached: bool) {}
    /// Private commit across every process this session owns, and how many
    /// processes that is.
    ///
    /// `None` means this domain cannot answer — a remote one holds no local job
    /// — and the caller falls back to reading the single supervised process,
    /// reporting no count rather than claiming the figure covers one process.
    fn memory_usage(&self) -> Option<crate::process_tree::JobUsage> {
        None
    }
}

/// Creates and supervises sessions in one execution domain.
pub trait AgentDomain: Send + Sync {
    fn spawn(
        &self,
        command: &ResolvedCommand,
        size: PtySize,
        environment: &[(String, String)],
        limits: crate::process_tree::JobLimits,
        on_output: OutputHandler,
    ) -> Result<Arc<dyn AgentSession>, DomainError>;
}

/// The built-in local execution domain backed by Windows ConPTY (or the
/// platform implementation supplied by `portable-pty`).
#[derive(Debug, Default)]
pub struct LocalPtyDomain;

impl AgentDomain for LocalPtyDomain {
    fn spawn(
        &self,
        command: &ResolvedCommand,
        size: PtySize,
        environment: &[(String, String)],
        limits: crate::process_tree::JobLimits,
        on_output: OutputHandler,
    ) -> Result<Arc<dyn AgentSession>, DomainError> {
        let session =
            PtySession::spawn_with_limits(command, size, environment, limits, on_output)?;
        Ok(Arc::new(session))
    }
}

impl AgentSession for PtySession {
    fn write(&self, bytes: &[u8]) -> Result<(), DomainError> {
        PtySession::write(self, bytes).map_err(DomainError::from)
    }

    fn resize(&self, size: PtySize) -> Result<(), DomainError> {
        PtySession::resize(self, size).map_err(DomainError::from)
    }

    fn pid(&self) -> Option<u32> {
        PtySession::pid(self)
    }

    fn try_wait(&self) -> Result<Option<u32>, DomainError> {
        PtySession::try_wait(self).map_err(DomainError::from)
    }

    fn wait_for_exit(&self) -> Result<u32, DomainError> {
        PtySession::wait_for_exit(self).map_err(DomainError::from)
    }

    fn kill(&self) -> Result<(), DomainError> {
        PtySession::kill(self).map_err(DomainError::from)
    }

    fn stop(&self) -> Result<StopOutcome, DomainError> {
        PtySession::stop(self).map_err(DomainError::from)
    }

    fn set_background(&self, background: bool) -> Result<(), DomainError> {
        PtySession::set_background(self, background).map_err(DomainError::from)
    }

    fn set_renderer_attached(&self, attached: bool) {
        PtySession::set_renderer_attached(self, attached);
    }

    fn memory_usage(&self) -> Option<crate::process_tree::JobUsage> {
        PtySession::memory_usage(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_domain_object_safe(_: &dyn AgentDomain) {}
    fn assert_session_object_safe<T: AgentSession>() {}

    #[test]
    fn local_domain_is_the_default_object_safe_implementation() {
        assert_domain_object_safe(&LocalPtyDomain);
        assert_session_object_safe::<PtySession>();
    }
}
