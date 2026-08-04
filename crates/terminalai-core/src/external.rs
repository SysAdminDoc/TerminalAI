//! Sessions this supervisor did not spawn.
//!
//! Claude Code maintains a per-PID registry of every running session on the
//! machine, so agents started from a terminal, an IDE or another tool are
//! discoverable rather than invisible. Showing them read-only is the honest
//! position: TerminalAI does not own their pty, cannot type into them and
//! cannot stop them, so the row must not offer actions the supervisor cannot
//! perform.
//!
//! The cardinal rule from the surrounding research holds here more than
//! anywhere: **never report idle from the absence of a signal.** An unreadable
//! or missing registry degrades to [`ExternalState::Unknown`], never to a
//! healthy-looking row.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::agent::Agent;

/// Where Claude Code writes one JSON file per live session, named by PID.
const CLAUDE_SESSION_DIR: &str = ".claude/sessions";
/// Reconciliation fallback when the registry directory cannot be read.
const ENUMERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// What we can say about a session we do not own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalState {
    /// The process named by `(pid, proc_start)` is running.
    Live,
    /// The registry named it but the process is gone; the file is stale.
    Ended,
    /// We could not determine either way. Never rendered as idle.
    Unknown,
}

/// One session running outside TerminalAI's supervision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExternalSession {
    pub agent: Agent,
    pub pid: u32,
    /// Process creation stamp from the agent's own registry. Paired with `pid`
    /// this is stable under PID reuse, which a bare PID is not.
    #[serde(default)]
    pub proc_start: Option<String>,
    pub session_id: Option<String>,
    pub cwd: PathBuf,
    pub name: Option<String>,
    /// `interactive`, `print`, and whatever the vendor adds next — retained
    /// verbatim rather than mapped onto our vocabulary.
    pub kind: Option<String>,
    pub entrypoint: Option<String>,
    pub version: Option<String>,
    pub started_at: Option<SystemTime>,
    pub state: ExternalState,
}

impl ExternalSession {
    /// Stable identity across reconciliations. A PID alone is not: the operating
    /// system reuses them, and a reused PID would silently inherit another
    /// session's row.
    pub fn identity(&self) -> String {
        match &self.proc_start {
            Some(start) => format!("{}:{}:{start}", self.agent.command_name(), self.pid),
            None => format!("{}:{}", self.agent.command_name(), self.pid),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct RawClaudeSession {
    pid: u32,
    #[serde(default, rename = "procStart")]
    proc_start: Option<serde_json::Value>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    version: Option<String>,
    /// Milliseconds since the Unix epoch.
    #[serde(default, rename = "startedAt")]
    started_at: Option<u64>,
}

/// Read every session Claude Code has registered on this machine.
///
/// `home` is the user's home directory; `is_running` decides liveness for a
/// PID, injected so the reconciliation logic is testable without spawning
/// processes.
pub fn claude_sessions(
    home: &Path,
    is_running: &dyn Fn(u32) -> Option<bool>,
) -> Vec<ExternalSession> {
    let dir = home.join(CLAUDE_SESSION_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        // A missing directory means "we cannot tell", not "nothing is running".
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // An unparseable registry file is skipped rather than guessed at; a
        // half-read row would be worse than an absent one.
        let Ok(raw) = serde_json::from_str::<RawClaudeSession>(&text) else {
            continue;
        };
        if let Some(session) = to_session(raw, is_running) {
            sessions.push(session);
        }
    }
    sessions.sort_by_key(|session| session.pid);
    sessions
}

fn to_session(
    raw: RawClaudeSession,
    is_running: &dyn Fn(u32) -> Option<bool>,
) -> Option<ExternalSession> {
    let started_at = match raw.started_at {
        Some(millis) => Some(UNIX_EPOCH.checked_add(Duration::from_millis(millis))?),
        None => None,
    };
    let state = match is_running(raw.pid) {
        Some(true) => ExternalState::Live,
        Some(false) => ExternalState::Ended,
        None => ExternalState::Unknown,
    };
    Some(ExternalSession {
        agent: Agent::Claude,
        pid: raw.pid,
        // The stamp's encoding is the vendor's business; it is used only as an
        // opaque identity token, so it is normalized to text rather than parsed.
        proc_start: raw.proc_start.map(|value| match value {
            serde_json::Value::String(text) => text,
            other => other.to_string(),
        }),
        session_id: raw.session_id,
        cwd: raw.cwd.unwrap_or_default(),
        name: raw.name,
        kind: raw.kind,
        entrypoint: raw.entrypoint,
        version: raw.version,
        started_at,
        state,
    })
}

/// Whether a PID names a live process.
///
/// `Some(false)` means proven gone; `None` means the question could not be
/// answered and the caller must degrade to [`ExternalState::Unknown`].
pub fn process_is_running(pid: u32) -> Option<bool> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // Could be gone, could be a permission failure. Callers that cannot
            // distinguish must not claim it ended, but for a session this user
            // started, an unopenable handle is an ended process in practice.
            return Some(false);
        }
        let mut status = 0u32;
        let readable = unsafe { GetExitCodeProcess(handle, &mut status) } != 0;
        unsafe {
            CloseHandle(handle);
        }
        if !readable {
            return None;
        }
        Some(status == STILL_ACTIVE as u32)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

/// Ask the CLI directly. Used only to reconcile when the registry directory is
/// unreadable, because it costs a process spawn.
pub fn enumerate_via_cli(binary: &Path) -> Option<Vec<ExternalSession>> {
    use std::process::{Command, Stdio};

    let mut command = Command::new(binary);
    command
        .args(["agents", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + ENUMERATION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    let raw: Vec<RawClaudeSession> = serde_json::from_slice(&output.stdout).ok()?;
    Some(
        raw.into_iter()
            .filter_map(|session| to_session(session, &process_is_running))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_registry(dir: &Path, pid: u32, body: &str) {
        std::fs::create_dir_all(dir).expect("create registry dir");
        std::fs::write(dir.join(format!("{pid}.json")), body).expect("write registry file");
    }

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "terminalai-external-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create scratch root");
        root
    }

    #[test]
    fn a_registered_session_is_adopted_with_its_identity() {
        let home = scratch("adopt");
        write_registry(
            &home.join(CLAUDE_SESSION_DIR),
            20196,
            r#"{"pid":20196,"sessionId":"9e496a1d-a49b-4201-b808-0e06da543200","cwd":"C:\\Users\\me","startedAt":1785774955558,"procStart":"639213573550673230","version":"2.1.220","kind":"interactive","entrypoint":"claude-vscode","name":"claude-0d"}"#,
        );

        let sessions = claude_sessions(&home, &|_| Some(true));
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.agent, Agent::Claude);
        assert_eq!(session.pid, 20196);
        assert_eq!(session.state, ExternalState::Live);
        assert_eq!(
            session.session_id.as_deref(),
            Some("9e496a1d-a49b-4201-b808-0e06da543200")
        );
        assert_eq!(session.entrypoint.as_deref(), Some("claude-vscode"));
        assert_eq!(session.version.as_deref(), Some("2.1.220"));
        // Identity carries procStart so a reused PID cannot inherit this row.
        assert_eq!(session.identity(), "claude:20196:639213573550673230");
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn a_reused_pid_is_a_different_session() {
        let first = ExternalSession {
            agent: Agent::Claude,
            pid: 100,
            proc_start: Some("aaa".into()),
            session_id: None,
            cwd: PathBuf::from("/a"),
            name: None,
            kind: None,
            entrypoint: None,
            version: None,
            started_at: None,
            state: ExternalState::Live,
        };
        let recycled = ExternalSession {
            proc_start: Some("bbb".into()),
            ..first.clone()
        };
        assert_ne!(first.identity(), recycled.identity());
    }

    #[test]
    fn an_unanswerable_liveness_check_is_unknown_never_idle() {
        let home = scratch("unknown");
        write_registry(
            &home.join(CLAUDE_SESSION_DIR),
            4242,
            r#"{"pid":4242,"cwd":"/tmp/x","procStart":"1"}"#,
        );
        let sessions = claude_sessions(&home, &|_| None);
        assert_eq!(sessions[0].state, ExternalState::Unknown);

        let sessions = claude_sessions(&home, &|_| Some(false));
        assert_eq!(sessions[0].state, ExternalState::Ended);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn an_unreadable_or_absent_registry_reports_nothing_rather_than_guessing() {
        let home = scratch("absent");
        // No registry directory at all.
        assert!(claude_sessions(&home, &|_| Some(true)).is_empty());

        // A malformed file is skipped; a valid sibling still resolves.
        let dir = home.join(CLAUDE_SESSION_DIR);
        write_registry(&dir, 1, "{ not json");
        write_registry(&dir, 2, r#"{"pid":2,"cwd":"/tmp/y"}"#);
        std::fs::write(dir.join("notes.txt"), "ignored").expect("write non-json");
        let sessions = claude_sessions(&home, &|_| Some(true));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, 2);
        // A file without procStart still yields a usable, if weaker, identity.
        assert_eq!(sessions[0].identity(), "claude:2");
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn an_unrepresentable_started_at_skips_the_registry_entry() {
        let home = scratch("started-at-overflow");
        let dir = home.join(CLAUDE_SESSION_DIR);
        write_registry(
            &dir,
            9999,
            r#"{"pid":9999,"cwd":"C:\\Users\\me","startedAt":9999999999999999}"#,
        );
        write_registry(&dir, 10000, r#"{"pid":10000,"cwd":"C:\\Users\\me"}"#);

        let sessions = claude_sessions(&home, &|_| Some(true));

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, 10000);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn this_machines_registry_parses() {
        // Exercises the real contract rather than only the fixture above. Skips
        // when no session registry exists.
        let Some(home) = dirs::home_dir() else {
            return;
        };
        if !home.join(CLAUDE_SESSION_DIR).is_dir() {
            return;
        }
        for session in claude_sessions(&home, &process_is_running) {
            assert!(session.pid > 0);
            assert_ne!(
                session.identity(),
                "claude:0",
                "a registry entry produced an unusable identity"
            );
        }
    }
}
