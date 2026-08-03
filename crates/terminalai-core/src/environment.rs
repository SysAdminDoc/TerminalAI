//! Per-session environment isolation.
//!
//! A session gets a deterministic block of ports and a small set of
//! lifecycle hooks. The registry runs setup before the agent starts and
//! teardown after it exits; the same values are exposed to both the hooks and
//! the agent through `TERMINALAI_*` variables.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use portable_pty::CommandBuilder;

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(windows)]
use crate::process_tree::ProcessJob;

pub const DEFAULT_PORT_BASE: u16 = 42_000;
pub const DEFAULT_PORT_COUNT: u16 = 4;
pub const PORT_BLOCK_STRIDE: u16 = 16;
pub const MAX_PORT_COUNT: u16 = PORT_BLOCK_STRIDE;
const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// User-configurable lifecycle and port settings for one launched session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentSpec {
    /// Shell command run in the project directory before the agent starts.
    #[serde(default)]
    pub setup: Option<String>,
    /// Shell command run in the project directory after the agent exits.
    #[serde(default)]
    pub teardown: Option<String>,
    /// First port in the deterministic session range.
    #[serde(default = "default_port_base")]
    pub port_base: u16,
    /// Number of ports assigned to each session. Zero disables allocation.
    #[serde(default = "default_port_count")]
    pub port_count: u16,
}

impl Default for EnvironmentSpec {
    fn default() -> Self {
        Self {
            setup: None,
            teardown: None,
            port_base: DEFAULT_PORT_BASE,
            port_count: DEFAULT_PORT_COUNT,
        }
    }
}

impl EnvironmentSpec {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.port_count > MAX_PORT_COUNT {
            return Err(EnvironmentError::PortCountTooLarge {
                requested: self.port_count,
                maximum: MAX_PORT_COUNT,
            });
        }
        if self.port_count > 0 && self.port_base < 1024 {
            return Err(EnvironmentError::PortBaseTooLow(self.port_base));
        }
        Ok(())
    }

    /// Allocate a stable, non-overlapping block from the session sequence.
    ///
    /// `s0001` receives the configured base, `s0002` starts one stride later,
    /// and so on. The stride leaves room for future per-session services while
    /// keeping the allocation independent of launch timing and queue order.
    pub fn ports_for_session(&self, session_id: &str) -> Result<Vec<u16>, EnvironmentError> {
        self.validate()?;
        if self.port_count == 0 {
            return Ok(Vec::new());
        }
        let sequence = session_id
            .strip_prefix('s')
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let first = u64::from(self.port_base)
            .saturating_add(sequence.saturating_mul(u64::from(PORT_BLOCK_STRIDE)));
        let last = first.saturating_add(u64::from(self.port_count) - 1);
        if last > u64::from(u16::MAX) {
            return Err(EnvironmentError::PortRangeExhausted {
                session_id: session_id.to_string(),
            });
        }
        Ok((0..self.port_count)
            .map(|offset| (first + u64::from(offset)) as u16)
            .collect())
    }
}

fn default_port_base() -> u16 {
    DEFAULT_PORT_BASE
}

fn default_port_count() -> u16 {
    DEFAULT_PORT_COUNT
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    #[error("port count {requested} exceeds the per-session maximum of {maximum}")]
    PortCountTooLarge { requested: u16, maximum: u16 },
    #[error("port base {0} must be at least 1024")]
    PortBaseTooLow(u16),
    #[error("deterministic port range is exhausted for session {session_id}")]
    PortRangeExhausted { session_id: String },
    #[error("could not start {phase} environment hook: {cause}")]
    HookSpawn { phase: &'static str, cause: String },
    #[error("{phase} environment hook exited unsuccessfully ({status:?})")]
    HookFailed {
        phase: &'static str,
        status: Option<i32>,
    },
    #[error("{phase} environment hook timed out after 30 seconds")]
    HookTimeout { phase: &'static str },
}

/// Environment variables shared by hooks and the agent process.
pub fn variables(session_id: &str, ports: &[u16]) -> Vec<(String, String)> {
    let port_list = ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut values = vec![
        ("TERMINALAI_SESSION_ID".to_string(), session_id.to_string()),
        ("TERMINALAI_PORTS".to_string(), port_list.clone()),
        (
            "TERMINALAI_PORT_BASE".to_string(),
            ports.first().map(u16::to_string).unwrap_or_default(),
        ),
    ];
    if let Some(port) = ports.first() {
        values.push(("PORT".to_string(), port.to_string()));
    }
    values
}

/// Run the configured setup hook, if any. A setup failure prevents the agent
/// from starting so a half-prepared worktree cannot look healthy.
pub fn run_setup(
    spec: &EnvironmentSpec,
    session_id: &str,
    cwd: &Path,
    ports: &[u16],
) -> Result<(), EnvironmentError> {
    run_hook("setup", spec.setup.as_deref(), session_id, cwd, ports)
}

/// Run the configured teardown hook. Callers intentionally decide whether a
/// teardown failure should affect the already-finished session.
pub fn run_teardown(
    spec: &EnvironmentSpec,
    session_id: &str,
    cwd: &Path,
    ports: &[u16],
) -> Result<(), EnvironmentError> {
    run_hook("teardown", spec.teardown.as_deref(), session_id, cwd, ports)
}

fn run_hook(
    phase: &'static str,
    command_line: Option<&str>,
    session_id: &str,
    cwd: &Path,
    ports: &[u16],
) -> Result<(), EnvironmentError> {
    run_hook_with_timeout(phase, command_line, session_id, cwd, ports, HOOK_TIMEOUT)
}

fn run_hook_with_timeout(
    phase: &'static str,
    command_line: Option<&str>,
    session_id: &str,
    cwd: &Path,
    ports: &[u16],
    timeout: Duration,
) -> Result<(), EnvironmentError> {
    let Some(command_line) = command_line
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let mut command = shell_command(command_line);
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_command_environment(&mut command, &variables(session_id, ports));
    let mut child = command
        .spawn()
        .map_err(|error| EnvironmentError::HookSpawn {
            phase,
            cause: error.to_string(),
        })?;
    #[cfg(windows)]
    let job = match ProcessJob::assign(child.as_raw_handle()) {
        Ok(job) => job,
        Err(cause) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EnvironmentError::HookSpawn { phase, cause });
        }
    };
    let deadline = Instant::now() + timeout;
    // One bounded wait on the child's own exit signal, rather than asking forty
    // times a second whether a hook that usually runs for minutes has finished.
    wait_for_child(&child, timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(EnvironmentError::HookFailed {
                        phase,
                        status: status.code(),
                    })
                };
            }
            Ok(None) if Instant::now() < deadline => {
                wait_for_child(&child, deadline.saturating_duration_since(Instant::now()))
            }
            Ok(None) => {
                #[cfg(windows)]
                let _ = job.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(EnvironmentError::HookTimeout { phase });
            }
            Err(error) => {
                #[cfg(windows)]
                let _ = job.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(EnvironmentError::HookSpawn {
                    phase,
                    cause: error.to_string(),
                });
            }
        }
    }
}

/// Block until `child` exits or `timeout` elapses, without periodic wakeups.
///
/// Windows signals a process handle at exit, so a single bounded wait replaces
/// the poll loop entirely. Elsewhere this degrades to one sleep, and the caller's
/// loop still bounds the total wait against its own deadline.
fn wait_for_child(child: &std::process::Child, timeout: Duration) {
    if timeout.is_zero() {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};

        let millis = u32::try_from(timeout.as_millis()).unwrap_or(INFINITE - 1);
        unsafe {
            WaitForSingleObject(child.as_raw_handle() as HANDLE, millis);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = child;
        std::thread::sleep(timeout.min(Duration::from_millis(25)));
    }
}

fn shell_command(command_line: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/s", "/c", command_line]);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("/bin/sh");
        command.args(["-lc", command_line]);
        command
    }
}

/// Apply the same secret-free environment baseline to direct helper commands
/// as the supervised PTY and environment hooks.
pub fn configure_command_environment(command: &mut Command, extra: &[(String, String)]) {
    command.env_clear();
    for key in safe_environment_keys() {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for (key, value) in extra {
        command.env(key, value);
    }
}

pub(crate) fn configure_pty_environment(builder: &mut CommandBuilder, extra: &[(String, String)]) {
    builder.env_clear();
    for key in safe_environment_keys() {
        if let Some(value) = std::env::var_os(key) {
            builder.env(key, value);
        }
    }
    for (key, value) in extra {
        builder.env(key, value);
    }
}

pub(crate) fn safe_environment_keys() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            "PATH",
            "SYSTEMROOT",
            "SystemDrive",
            "windir",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "HOMEDRIVE",
            "HOMEPATH",
            "COMSPEC",
            "PATHEXT",
            "NUMBER_OF_PROCESSORS",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "NO_PROXY",
        ]
    }
    #[cfg(not(windows))]
    {
        &["PATH", "HOME", "TMPDIR", "TERM", "LANG", "SHELL"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_deterministic_and_leave_a_stride_between_sessions() {
        let spec = EnvironmentSpec::default();
        assert_eq!(
            spec.ports_for_session("s0001").unwrap(),
            [42_000, 42_001, 42_002, 42_003]
        );
        assert_eq!(
            spec.ports_for_session("s0002").unwrap(),
            [42_016, 42_017, 42_018, 42_019]
        );
        assert_eq!(
            spec.ports_for_session("s0001").unwrap(),
            spec.ports_for_session("s0001").unwrap()
        );
    }

    #[test]
    fn environment_variables_expose_the_block_without_secrets() {
        let values = variables("s0007", &[42_096, 42_097]);
        assert!(values.contains(&("TERMINALAI_SESSION_ID".into(), "s0007".into())));
        assert!(values.contains(&("TERMINALAI_PORTS".into(), "42096,42097".into())));
        assert!(values.contains(&("PORT".into(), "42096".into())));
        assert!(!values
            .iter()
            .any(|(key, _)| key.contains("KEY") || key.contains("TOKEN")));
    }

    #[test]
    fn invalid_port_configuration_is_rejected() {
        let spec = EnvironmentSpec {
            port_count: MAX_PORT_COUNT + 1,
            ..Default::default()
        };
        assert!(matches!(
            spec.ports_for_session("s0001"),
            Err(EnvironmentError::PortCountTooLarge { .. })
        ));
    }

    #[test]
    fn successful_hook_receives_the_session_environment() {
        let spec = EnvironmentSpec {
            setup: Some(
                if cfg!(windows) {
                    "if \"%TERMINALAI_SESSION_ID%\"==\"s0001\" exit /b 0"
                } else {
                    "test \"$TERMINALAI_SESSION_ID\" = s0001"
                }
                .into(),
            ),
            ..Default::default()
        };
        run_setup(&spec, "s0001", Path::new("."), &[42_000]).expect("setup hook");
    }

    #[cfg(windows)]
    #[test]
    fn timed_out_hook_terminates_descendant_processes() {
        let marker = std::env::temp_dir().join(format!(
            "terminalai-hook-timeout-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&marker);
        let command = format!(
            r#"start "" /b cmd.exe /d /c "ping -n 30 127.0.0.1 > nul & echo leaked > {}" & ping -n 30 127.0.0.1 > nul"#,
            marker.display()
        );

        let result = run_hook_with_timeout(
            "setup",
            Some(&command),
            "s0001",
            Path::new("."),
            &[],
            Duration::from_millis(100),
        );
        assert!(matches!(result, Err(EnvironmentError::HookTimeout { .. })));
        std::thread::sleep(Duration::from_millis(500));
        assert!(!marker.exists(), "hook descendant survived timeout");
        let _ = std::fs::remove_file(marker);
    }

    #[cfg(windows)]
    #[test]
    fn resolved_agents_run_with_the_sanitized_environment() {
        use crate::agent::{self, Agent};

        for key in [
            "APPDATA",
            "LOCALAPPDATA",
            "HOMEDRIVE",
            "HOMEPATH",
            "TMP",
            "SystemDrive",
            "windir",
            "NUMBER_OF_PROCESSORS",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "NO_PROXY",
        ] {
            assert!(
                safe_environment_keys().contains(&key),
                "allowlist omitted {key}"
            );
        }

        const SENTINEL: &str = "TERMINALAI_R41_PARENT_SECRET";
        std::env::set_var(SENTINEL, "must-not-cross-process-boundary");
        for agent in [Agent::Claude, Agent::Codex] {
            let binary = agent::resolve(agent, None).expect("installed agent binary");
            let mut command = Command::new(&binary.path);
            command.arg("--version");
            configure_command_environment(&mut command, &[]);
            let output = command.output().expect("run agent version command");
            assert!(
                output.status.success(),
                "{} --version failed: {}{}",
                agent.command_name(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", "set"]);
        configure_command_environment(&mut command, &[]);
        let output = command.output().expect("inspect sanitized environment");
        std::env::remove_var(SENTINEL);
        let child_environment = String::from_utf8_lossy(&output.stdout);
        assert!(
            !child_environment.contains(SENTINEL),
            "parent sentinel crossed the allowlist: {child_environment}"
        );
    }
}
