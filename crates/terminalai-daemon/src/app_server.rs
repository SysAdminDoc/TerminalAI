//! Opt-in Codex app-server stdio transport.
//!
//! This module intentionally does not participate in the default daemon
//! process. The upstream API is experimental, so callers must enable the
//! `codex-app-server` Cargo feature and explicitly spawn an adapter. The
//! process wrapper is transport-only: the core parser owns protocol
//! compatibility and the normal daemon registry remains the event sink.

use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use terminalai_core::agent::{self, Agent};
use terminalai_core::{
    AgentEvent, AppServerEvent, AppServerMessage, AppServerNotification, AppServerParseError,
    AppServerRequest, AppServerResponse,
};

#[derive(Debug, Clone, Default)]
pub struct CodexAppServerConfig {
    pub configured_path: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum AppServerError {
    #[error("could not resolve Codex app-server executable: {0}")]
    Resolve(#[from] terminalai_core::ResolveError),
    #[error("Codex app-server process failed: {0}")]
    Io(#[from] io::Error),
    #[error("Codex app-server JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex app-server message failed validation: {0}")]
    Parse(#[from] AppServerParseError),
    #[error("Codex app-server did not provide a stdin pipe")]
    MissingStdin,
    #[error("Codex app-server did not provide a stdout pipe")]
    MissingStdout,
}

pub struct CodexAppServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CodexAppServer {
    /// Spawn `codex app-server` on the explicit stdio transport.
    pub fn spawn(config: CodexAppServerConfig) -> Result<Self, AppServerError> {
        let binary = agent::resolve(Agent::Codex, config.configured_path.as_deref())?;
        let mut command = Command::new(binary.path);
        command
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Keep the adapter from writing into the daemon's console. A
            // future diagnostics surface can add a dedicated stderr reader.
            .stderr(Stdio::null());
        if let Some(cwd) = config.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or(AppServerError::MissingStdin)?;
        let stdout = child.stdout.take().ok_or(AppServerError::MissingStdout)?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    pub fn send(&mut self, request: &AppServerRequest) -> Result<(), AppServerError> {
        self.write_line(request.encode_line()?)
    }

    pub fn notify(&mut self, notification: &AppServerNotification) -> Result<(), AppServerError> {
        self.write_line(notification.encode_line()?)
    }

    pub fn respond(&mut self, response: &AppServerResponse) -> Result<(), AppServerError> {
        self.write_line(response.encode_line()?)
    }

    /// Read the next protocol message. Responses are returned so callers that
    /// issue requests can correlate them; [`Self::next_event`] skips them.
    pub fn next_message(&mut self) -> Result<Option<AppServerMessage>, AppServerError> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(terminalai_core::parse_app_server_message(
            line.trim_end_matches(['\r', '\n']),
        )?))
    }

    /// Read until the server emits a notification or server request.
    pub fn next_event(&mut self) -> Result<Option<AppServerEvent>, AppServerError> {
        loop {
            match self.next_message()? {
                Some(AppServerMessage::Notification { event })
                | Some(AppServerMessage::Request { event, .. }) => return Ok(Some(event)),
                Some(AppServerMessage::Response { .. }) => {}
                None => return Ok(None),
            }
        }
    }

    pub fn next_agent_event(&mut self) -> Result<Option<AgentEvent>, AppServerError> {
        Ok(self.next_event()?.map(AgentEvent::AppServer))
    }

    pub fn process_id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, AppServerError> {
        Ok(self.child.try_wait()?)
    }

    pub fn kill(&mut self) -> Result<(), AppServerError> {
        self.child.kill().map_err(AppServerError::Io)
    }

    fn write_line(&mut self, line: String) -> Result<(), AppServerError> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for CodexAppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_gated_transport_exposes_typed_commands_without_spawning() {
        let request = AppServerRequest::interrupt(4, "thr-1", "turn-1");
        let json: serde_json::Value =
            serde_json::from_str(&request.encode_line().expect("encode request"))
                .expect("request JSON");
        assert_eq!(json["method"], "turn/interrupt");
        assert_eq!(json["params"]["threadId"], "thr-1");
        assert_eq!(json["params"]["turnId"], "turn-1");
    }

    #[test]
    fn initialized_notification_is_available_for_handshake() {
        let notification = AppServerNotification::initialized();
        let json: serde_json::Value =
            serde_json::from_str(&notification.encode_line().expect("encode notification"))
                .expect("notification JSON");
        assert_eq!(json["method"], "initialized");
        assert_eq!(json["params"], serde_json::json!({}));
    }
}
