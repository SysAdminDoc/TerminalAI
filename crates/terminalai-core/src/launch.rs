//! Turning launcher choices into an exact argument vector.
//!
//! Every field here maps to a flag verified against Claude Code 2.1.170 and
//! codex-cli 0.146.0. The two CLIs express the same concepts differently —
//! Claude takes `--effort`, Codex takes a config override; Claude inherits the
//! working directory, Codex wants `--cd` — so the mapping is explicit per agent
//! rather than a shared flag table.

use std::path::{Path, PathBuf};

use crate::agent::{Agent, AgentBinary};

/// Reasoning effort. Claude accepts all five; Codex accepts the first four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// How much the agent may do without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// Claude `default` / Codex `on-request`.
    Ask,
    /// Claude `plan` — read and propose, never edit. No Codex equivalent, so
    /// Codex sessions pair this with the read-only sandbox.
    Plan,
    /// Claude `acceptEdits` / Codex `untrusted`.
    AcceptEdits,
    /// Claude `bypassPermissions` / Codex `never`.
    Bypass,
}

/// Codex-only filesystem policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sandbox {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl Sandbox {
    fn as_str(self) -> &'static str {
        match self {
            Sandbox::ReadOnly => "read-only",
            Sandbox::WorkspaceWrite => "workspace-write",
            Sandbox::DangerFullAccess => "danger-full-access",
        }
    }
}

/// Whether this is a fresh session or picks up an old one.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Resume {
    #[default]
    New,
    /// Most recent session in this folder.
    Last,
    /// A specific session id.
    Session(String),
    /// Branch off a session, leaving the original intact.
    Fork(String),
}

/// Everything the launcher dialog collects.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LaunchSpec {
    pub agent: Agent,
    /// Display name for the fleet row. Claude gets it via `--name`; for Codex
    /// it is TerminalAI-side only.
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub permission: Option<Permission>,
    /// Codex only.
    pub sandbox: Option<Sandbox>,
    /// Codex only — layers `$CODEX_HOME/<name>.config.toml`.
    pub profile: Option<String>,
    pub add_dirs: Vec<PathBuf>,
    pub resume: Resume,
    /// Claude only.
    pub max_budget_usd: Option<f64>,
    /// Codex only.
    pub web_search: bool,
    /// Sent as the first turn. Both CLIs take it as a positional argument.
    pub initial_prompt: Option<String>,
    /// Escape hatch for flags the launcher does not model yet.
    pub extra_args: Vec<String>,
}

impl Default for Agent {
    fn default() -> Self {
        Agent::Claude
    }
}

/// A command ready to hand to the PTY layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl ResolvedCommand {
    /// Human-readable form for the UI's "what will run" preview. Quoting here is
    /// for display only — the real spawn passes `args` as a vector, so nothing
    /// downstream has to re-parse this string.
    pub fn preview(&self) -> String {
        let mut out = quote_for_display(&self.program.to_string_lossy());
        for a in &self.args {
            out.push(' ');
            out.push_str(&quote_for_display(a));
        }
        out
    }
}

fn quote_for_display(s: &str) -> String {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("working directory does not exist: {0}")]
    MissingCwd(PathBuf),
    #[error("{flag} is not supported by {agent}")]
    Unsupported { flag: &'static str, agent: &'static str },
    #[error("{0} cannot resume a specific session id from the command line; use Last or New")]
    ResumeUnsupported(&'static str),
}

impl LaunchSpec {
    /// Build the argument vector. Returns an error rather than silently dropping
    /// a field the chosen agent cannot express — a launcher that quietly ignores
    /// "read-only sandbox" is worse than one that refuses.
    pub fn resolve(&self, binary: &AgentBinary) -> Result<ResolvedCommand, LaunchError> {
        if !self.cwd.is_dir() {
            return Err(LaunchError::MissingCwd(self.cwd.clone()));
        }
        let args = match self.agent {
            Agent::Claude => self.claude_args()?,
            Agent::Codex => self.codex_args()?,
        };
        Ok(ResolvedCommand { program: binary.path.clone(), args, cwd: self.cwd.clone() })
    }

    fn claude_args(&self) -> Result<Vec<String>, LaunchError> {
        if self.sandbox.is_some() {
            return Err(LaunchError::Unsupported { flag: "--sandbox", agent: "Claude Code" });
        }
        if self.profile.is_some() {
            return Err(LaunchError::Unsupported { flag: "--profile", agent: "Claude Code" });
        }
        if self.web_search {
            return Err(LaunchError::Unsupported { flag: "--search", agent: "Claude Code" });
        }

        let mut a = Vec::new();
        // Claude inherits its working directory from the process, so `cwd` is
        // applied at spawn time rather than as a flag.
        if let Some(m) = &self.model {
            a.push("--model".into());
            a.push(m.clone());
        }
        if let Some(e) = self.effort {
            a.push("--effort".into());
            a.push(e.as_str().into());
        }
        if let Some(p) = self.permission {
            a.push("--permission-mode".into());
            a.push(
                match p {
                    Permission::Ask => "default",
                    Permission::Plan => "plan",
                    Permission::AcceptEdits => "acceptEdits",
                    Permission::Bypass => "bypassPermissions",
                }
                .into(),
            );
        }
        if let Some(n) = &self.name {
            a.push("--name".into());
            a.push(n.clone());
        }
        for d in &self.add_dirs {
            a.push("--add-dir".into());
            a.push(d.to_string_lossy().into_owned());
        }
        if let Some(b) = self.max_budget_usd {
            a.push("--max-budget-usd".into());
            a.push(format_usd(b));
        }
        match &self.resume {
            Resume::New => {}
            Resume::Last => a.push("--continue".into()),
            Resume::Session(id) => {
                a.push("--resume".into());
                a.push(id.clone());
            }
            Resume::Fork(id) => {
                a.push("--resume".into());
                a.push(id.clone());
                a.push("--fork-session".into());
            }
        }
        a.extend(self.extra_args.iter().cloned());
        if let Some(p) = &self.initial_prompt {
            a.push(p.clone());
        }
        Ok(a)
    }

    fn codex_args(&self) -> Result<Vec<String>, LaunchError> {
        if self.max_budget_usd.is_some() {
            return Err(LaunchError::Unsupported { flag: "--max-budget-usd", agent: "Codex" });
        }
        if matches!(self.permission, Some(Permission::Plan)) {
            // Codex has no plan mode; the honest equivalent is an explicit
            // read-only sandbox, which the caller must choose deliberately.
            return Err(LaunchError::Unsupported { flag: "plan mode", agent: "Codex" });
        }

        let mut a = Vec::new();
        // Subcommands come first for the resume family.
        match &self.resume {
            Resume::New => {}
            Resume::Last => {
                a.push("resume".into());
                a.push("--last".into());
            }
            Resume::Session(id) => {
                a.push("resume".into());
                a.push(id.clone());
            }
            Resume::Fork(id) => {
                a.push("fork".into());
                a.push(id.clone());
            }
        }
        if let Some(m) = &self.model {
            a.push("--model".into());
            a.push(m.clone());
        }
        if let Some(e) = self.effort {
            // Not a flag — a config override, parsed as TOML.
            a.push("--config".into());
            a.push(format!("model_reasoning_effort=\"{}\"", e.as_str()));
        }
        if let Some(p) = self.permission {
            a.push("--ask-for-approval".into());
            a.push(
                match p {
                    Permission::Ask => "on-request",
                    Permission::AcceptEdits => "untrusted",
                    Permission::Bypass => "never",
                    Permission::Plan => unreachable!("rejected above"),
                }
                .into(),
            );
        }
        if let Some(s) = self.sandbox {
            a.push("--sandbox".into());
            a.push(s.as_str().into());
        }
        if let Some(p) = &self.profile {
            a.push("--profile".into());
            a.push(p.clone());
        }
        // Codex does not inherit cwd for its workspace root; it must be told.
        a.push("--cd".into());
        a.push(self.cwd.to_string_lossy().into_owned());
        for d in &self.add_dirs {
            a.push("--add-dir".into());
            a.push(d.to_string_lossy().into_owned());
        }
        if self.web_search {
            a.push("--search".into());
        }
        a.extend(self.extra_args.iter().cloned());
        if let Some(p) = &self.initial_prompt {
            a.push(p.clone());
        }
        Ok(a)
    }
}

/// `--max-budget-usd` wants a plain decimal, not scientific notation.
fn format_usd(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Effort levels the launcher should offer for an agent.
pub fn supported_efforts(agent: Agent) -> &'static [Effort] {
    match agent {
        Agent::Claude => {
            &[Effort::Low, Effort::Medium, Effort::High, Effort::XHigh, Effort::Max]
        }
        Agent::Codex => &[Effort::Low, Effort::Medium, Effort::High, Effort::XHigh],
    }
}

/// Convenience for callers that only have a path.
pub fn spec_for(agent: Agent, cwd: &Path) -> LaunchSpec {
    LaunchSpec { agent, cwd: cwd.to_path_buf(), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Origin;

    fn binary(agent: Agent) -> AgentBinary {
        AgentBinary { agent, path: PathBuf::from("x"), origin: Origin::Configured }
    }

    fn spec(agent: Agent) -> LaunchSpec {
        LaunchSpec { agent, cwd: std::env::temp_dir(), ..Default::default() }
    }

    #[test]
    fn claude_full_house() {
        let s = LaunchSpec {
            model: Some("opus".into()),
            effort: Some(Effort::XHigh),
            permission: Some(Permission::Plan),
            name: Some("api rewrite".into()),
            max_budget_usd: Some(5.0),
            ..spec(Agent::Claude)
        };
        let c = s.resolve(&binary(Agent::Claude)).unwrap();
        assert_eq!(
            c.args,
            [
                "--model",
                "opus",
                "--effort",
                "xhigh",
                "--permission-mode",
                "plan",
                "--name",
                "api rewrite",
                "--max-budget-usd",
                "5"
            ]
        );
    }

    #[test]
    fn codex_effort_is_a_config_override_not_a_flag() {
        let s = LaunchSpec {
            model: Some("gpt-5.1-codex".into()),
            effort: Some(Effort::High),
            sandbox: Some(Sandbox::WorkspaceWrite),
            ..spec(Agent::Codex)
        };
        let c = s.resolve(&binary(Agent::Codex)).unwrap();
        assert!(c.args.windows(2).any(|w| w
            == ["--config".to_string(), "model_reasoning_effort=\"high\"".to_string()]));
        assert!(c.args.contains(&"--cd".to_string()), "codex must be told its workspace root");
    }

    #[test]
    fn codex_resume_is_a_subcommand_and_comes_first() {
        let s = LaunchSpec { resume: Resume::Last, ..spec(Agent::Codex) };
        let c = s.resolve(&binary(Agent::Codex)).unwrap();
        assert_eq!(&c.args[..2], ["resume", "--last"]);
    }

    #[test]
    fn claude_fork_keeps_the_original() {
        let s = LaunchSpec { resume: Resume::Fork("abc-123".into()), ..spec(Agent::Claude) };
        let c = s.resolve(&binary(Agent::Claude)).unwrap();
        assert_eq!(c.args, ["--resume", "abc-123", "--fork-session"]);
    }

    #[test]
    fn unsupported_options_are_refused_not_dropped() {
        let s = LaunchSpec { sandbox: Some(Sandbox::ReadOnly), ..spec(Agent::Claude) };
        assert!(matches!(
            s.resolve(&binary(Agent::Claude)),
            Err(LaunchError::Unsupported { flag: "--sandbox", .. })
        ));

        let s = LaunchSpec { permission: Some(Permission::Plan), ..spec(Agent::Codex) };
        assert!(matches!(
            s.resolve(&binary(Agent::Codex)),
            Err(LaunchError::Unsupported { flag: "plan mode", .. })
        ));
    }

    #[test]
    fn prompt_is_positional_and_last() {
        let s = LaunchSpec {
            initial_prompt: Some("fix the & in the parser".into()),
            model: Some("sonnet".into()),
            ..spec(Agent::Claude)
        };
        let c = s.resolve(&binary(Agent::Claude)).unwrap();
        assert_eq!(c.args.last().unwrap(), "fix the & in the parser");
    }

    #[test]
    fn missing_cwd_fails_before_spawn() {
        let s = LaunchSpec { cwd: PathBuf::from("/nope/nope"), ..spec(Agent::Claude) };
        assert!(matches!(s.resolve(&binary(Agent::Claude)), Err(LaunchError::MissingCwd(_))));
    }

    #[test]
    fn budget_is_plain_decimal() {
        assert_eq!(format_usd(5.0), "5");
        assert_eq!(format_usd(0.5), "0.5");
        assert_eq!(format_usd(12.25), "12.25");
    }
}
