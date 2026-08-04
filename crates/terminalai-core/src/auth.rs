//! Whether an agent can still authenticate.
//!
//! Running many sessions at once provokes OAuth refresh races, and an agent
//! whose credentials have expired looks exactly like a busy one from the
//! outside: it accepts a prompt, works for a moment, and fails. Without a
//! fleet-level signal the operator watches a work queue fail one entry at a
//! time and learns nothing from any of them.
//!
//! The cardinal rule from `external` applies here too, and harder: **never
//! report healthy from the absence of a signal.** A probe that cannot be run,
//! times out, or answers in a shape this module does not model degrades to
//! [`AuthState::Unknown`], which holds nothing and blocks nothing. Only an
//! explicit "not logged in" is treated as expired.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::agent::Agent;

/// How long an auth probe may take before it is abandoned.
const AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthState {
    /// The agent reported working credentials.
    Authenticated,
    /// The agent reported that it is not logged in.
    Expired,
    /// The question could not be answered. Never rendered as healthy, and never
    /// used to hold work: an unreachable probe must not stop the fleet.
    Unknown,
}

/// What one agent said about its credentials.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentAuth {
    pub agent: Agent,
    pub state: AuthState,
    /// The account the agent named, when it named one. Shown so an operator with
    /// several logins can see which one the fleet is using.
    #[serde(default)]
    pub account: Option<String>,
    /// Why the state is `Unknown`, when there is something to say.
    #[serde(default)]
    pub detail: Option<String>,
}

impl AgentAuth {
    pub fn unknown(agent: Agent, detail: impl Into<String>) -> Self {
        Self {
            agent,
            state: AuthState::Unknown,
            account: None,
            detail: Some(detail.into()),
        }
    }
}

/// Read Claude Code's `auth status --json`.
///
/// The shape is `{"loggedIn": bool, "email": "...", "subscriptionType": "..."}`.
/// A payload missing `loggedIn` is `Unknown`, not expired: a schema change must
/// not present itself as a logged-out fleet.
pub fn parse_claude(stdout: &str) -> AgentAuth {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return AgentAuth::unknown(Agent::Claude, "auth status did not answer with JSON");
    };
    let Some(logged_in) = value.get("loggedIn").and_then(serde_json::Value::as_bool) else {
        return AgentAuth::unknown(Agent::Claude, "auth status JSON has no loggedIn field");
    };
    let account = value
        .get("email")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    AgentAuth {
        agent: Agent::Claude,
        state: if logged_in {
            AuthState::Authenticated
        } else {
            AuthState::Expired
        },
        account,
        detail: None,
    }
}

/// Read Codex's `login status`, which answers in prose rather than JSON.
///
/// Matching is deliberately narrow. "Logged in using ChatGPT" is the observed
/// success line; an explicit not-logged-in line is the observed failure. Any
/// other wording is `Unknown`, because guessing from unfamiliar prose is how a
/// working fleet gets told it is logged out.
pub fn parse_codex(stdout: &str, exit_ok: bool) -> AgentAuth {
    let text = stdout.trim().to_ascii_lowercase();
    if text.contains("not logged in") || text.contains("please run") && text.contains("login") {
        return AgentAuth {
            agent: Agent::Codex,
            state: AuthState::Expired,
            account: None,
            detail: None,
        };
    }
    if exit_ok && text.contains("logged in") {
        let account = stdout
            .trim()
            .strip_prefix("Logged in using ")
            .map(|rest| rest.trim().to_owned());
        return AgentAuth {
            agent: Agent::Codex,
            state: AuthState::Authenticated,
            account,
            detail: None,
        };
    }
    AgentAuth::unknown(Agent::Codex, "login status did not say whether it is logged in")
}

/// The arguments each agent's status command takes.
fn status_args(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["auth", "status", "--json"],
        Agent::Codex => &["login", "status"],
    }
}

/// Ask one resolved agent binary whether it is still authenticated.
pub fn probe(agent: Agent, path: &Path) -> AgentAuth {
    let mut command = Command::new(path);
    command
        .args(status_args(agent))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = command.spawn() else {
        return AgentAuth::unknown(agent, "could not run the agent's status command");
    };
    let deadline = Instant::now() + AUTH_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                return AgentAuth::unknown(agent, "could not wait for the status command");
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return AgentAuth::unknown(
                agent,
                format!(
                    "status did not answer within {}s",
                    AUTH_PROBE_TIMEOUT.as_secs()
                ),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let Ok(output) = child.wait_with_output() else {
        return AgentAuth::unknown(agent, "could not read the status command output");
    };
    // `codex login status` writes its answer to **stderr** and exits 0, while
    // `claude auth status --json` writes JSON to stdout. Reading stdout alone
    // reports Codex as `Unknown` forever; combining them unconditionally would
    // let a stray Claude warning corrupt the JSON. So: stdout when it has
    // anything to say, stderr otherwise.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stdout.trim().is_empty() {
        String::from_utf8_lossy(&output.stderr).into_owned()
    } else {
        stdout.into_owned()
    };
    match agent {
        Agent::Claude => parse_claude(&text),
        Agent::Codex => parse_codex(&text, output.status.success()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_reports_its_account_when_logged_in() {
        let auth = parse_claude(
            r#"{"loggedIn":true,"authMethod":"claude.ai","email":"someone@example.com","subscriptionType":"max"}"#,
        );
        assert_eq!(auth.state, AuthState::Authenticated);
        assert_eq!(auth.account.as_deref(), Some("someone@example.com"));
    }

    #[test]
    fn claude_reports_expired_only_on_an_explicit_false() {
        assert_eq!(parse_claude(r#"{"loggedIn":false}"#).state, AuthState::Expired);
    }

    #[test]
    fn a_claude_schema_change_is_unknown_not_logged_out() {
        // A renamed field must not present itself as a logged-out fleet.
        assert_eq!(
            parse_claude(r#"{"authenticated":true}"#).state,
            AuthState::Unknown
        );
        assert_eq!(parse_claude("not json at all").state, AuthState::Unknown);
        assert_eq!(parse_claude("").state, AuthState::Unknown);
    }

    #[test]
    fn codex_reads_its_prose_success_line() {
        let auth = parse_codex("Logged in using ChatGPT", true);
        assert_eq!(auth.state, AuthState::Authenticated);
        assert_eq!(auth.account.as_deref(), Some("ChatGPT"));
    }

    #[test]
    fn codex_reports_expired_on_an_explicit_line() {
        assert_eq!(
            parse_codex("Not logged in. Please run `codex login`.", false).state,
            AuthState::Expired
        );
    }

    #[test]
    fn unfamiliar_codex_wording_is_unknown() {
        // Guessing from prose this module has never seen is how a working fleet
        // gets told it is logged out.
        assert_eq!(parse_codex("Session refreshed", true).state, AuthState::Unknown);
        assert_eq!(parse_codex("", false).state, AuthState::Unknown);
    }

    #[test]
    fn a_success_line_with_a_failing_exit_is_not_trusted() {
        assert_eq!(parse_codex("Logged in using ChatGPT", false).state, AuthState::Unknown);
    }
}
