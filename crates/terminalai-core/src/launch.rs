//! Turning launcher choices into an exact argument vector.
//!
//! Every field here maps to a flag verified against Claude Code 2.1.170 and
//! codex-cli 0.146.0. Runtime capability probing supplies current model and
//! effort catalogs; the two CLIs express the same concepts differently —
//! Claude takes `--effort`, Codex takes a config override; Claude inherits the
//! working directory, Codex wants `--cd` — so the mapping is explicit per agent
//! rather than a shared flag table.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::agent::{Agent, AgentBinary};
use crate::environment::{EnvironmentError, EnvironmentSpec};

/// Reasoning effort. Known values remain convenient constants, while a runtime
/// can add a new string before this binary is updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Custom(String),
}

impl Effort {
    pub fn as_str(&self) -> &str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
            Effort::Custom(value) => value,
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom(_))
    }
}

impl Serialize for Effort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Effort {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::XHigh,
            "max" => Self::Max,
            _ => Self::Custom(value),
        })
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
#[serde(tag = "kind", content = "id", rename_all = "kebab-case")]
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

/// Native session ids are passed as command-line tokens on both supported
/// agents. Keep the accepted shape deliberately narrow so an id can never
/// become a provider flag or a multi-token config override.
pub fn is_valid_resume_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 128
        && first.is_ascii_alphanumeric()
        && chars.all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | ':' | '-')
        })
}

/// Everything the launcher dialog collects.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LaunchSpec {
    pub agent: Agent,
    /// Display name for the fleet row. Claude gets it via `--name`; for Codex
    /// it is TerminalAI-side only.
    pub name: Option<String>,
    pub cwd: PathBuf,
    /// Per-session setup/teardown hooks and deterministic service ports.
    #[serde(default)]
    pub environment: EnvironmentSpec,
    /// Give this session its own Git worktree and branch instead of sharing the
    /// repository's working tree with every other session on it.
    #[serde(default)]
    pub worktree: bool,
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
    /// Trusted-input-only escape hatch for flags the launcher does not model
    /// yet. Never populate this from pasted prompt text.
    pub extra_args: Vec<String>,
}

/// A command ready to hand to the PTY layer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    Unsupported {
        flag: &'static str,
        agent: &'static str,
    },
    #[error("{0} cannot resume a specific session id from the command line; use Last or New")]
    ResumeUnsupported(&'static str),
    #[error("resume session id has an invalid command-line shape")]
    InvalidResumeId,
    #[error("max_budget_usd must be finite and non-negative")]
    InvalidBudget,
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
}

impl LaunchSpec {
    /// Build the argument vector. Returns an error rather than silently dropping
    /// a field the chosen agent cannot express — a launcher that quietly ignores
    /// "read-only sandbox" is worse than one that refuses.
    pub fn resolve(&self, binary: &AgentBinary) -> Result<ResolvedCommand, LaunchError> {
        if !self.cwd.is_dir() {
            return Err(LaunchError::MissingCwd(self.cwd.clone()));
        }
        if let Some(budget) = self.max_budget_usd {
            if !budget.is_finite() || budget < 0.0 {
                return Err(LaunchError::InvalidBudget);
            }
        }
        if matches!(&self.resume, Resume::Session(id) | Resume::Fork(id) if !is_valid_resume_id(id))
        {
            return Err(LaunchError::InvalidResumeId);
        }
        self.environment.validate()?;
        let args = match self.agent {
            Agent::Claude => self.claude_args()?,
            Agent::Codex => self.codex_args()?,
        };
        Ok(ResolvedCommand {
            program: binary.path.clone(),
            args,
            cwd: self.cwd.clone(),
        })
    }

    fn claude_args(&self) -> Result<Vec<String>, LaunchError> {
        if self.sandbox.is_some() {
            return Err(LaunchError::Unsupported {
                flag: "--sandbox",
                agent: "Claude Code",
            });
        }
        if self.profile.is_some() {
            return Err(LaunchError::Unsupported {
                flag: "--profile",
                agent: "Claude Code",
            });
        }
        if self.web_search {
            return Err(LaunchError::Unsupported {
                flag: "--search",
                agent: "Claude Code",
            });
        }

        let mut a = Vec::new();
        // Claude inherits its working directory from the process, so `cwd` is
        // applied at spawn time rather than as a flag.
        if let Some(m) = &self.model {
            a.push("--model".into());
            a.push(m.clone());
        }
        if let Some(e) = self.effort.as_ref() {
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
            a.push("--".into());
            a.push(p.clone());
        }
        Ok(a)
    }

    fn codex_args(&self) -> Result<Vec<String>, LaunchError> {
        if self.max_budget_usd.is_some() {
            return Err(LaunchError::Unsupported {
                flag: "--max-budget-usd",
                agent: "Codex",
            });
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
        if let Some(e) = self.effort.as_ref() {
            // Not a flag — a config override, parsed as TOML.
            a.push("--config".into());
            a.push(format!("model_reasoning_effort=\"{}\"", e.as_str()));
        }
        if self.permission == Some(Permission::Plan) {
            // Codex exposes its collaboration mode as a TOML config override.
            a.push("--config".into());
            a.push("collaboration_mode.mode=\"Plan\"".into());
        } else if let Some(p) = self.permission {
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
            a.push("--".into());
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

/// Convenience for callers that only have a path.
pub fn spec_for(agent: Agent, cwd: &Path) -> LaunchSpec {
    LaunchSpec {
        agent,
        cwd: cwd.to_path_buf(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Origin;

    fn binary(agent: Agent) -> AgentBinary {
        AgentBinary {
            agent,
            path: PathBuf::from("x"),
            origin: Origin::Configured,
        }
    }

    fn spec(agent: Agent) -> LaunchSpec {
        LaunchSpec {
            agent,
            cwd: std::env::temp_dir(),
            ..Default::default()
        }
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
            == [
                "--config".to_string(),
                "model_reasoning_effort=\"high\"".to_string()
            ]));
        assert!(
            c.args.contains(&"--cd".to_string()),
            "codex must be told its workspace root"
        );
    }

    #[test]
    fn codex_passes_through_runtime_effort() {
        let s = LaunchSpec {
            effort: Some(Effort::Custom("ultra".into())),
            ..spec(Agent::Codex)
        };
        let c = s.resolve(&binary(Agent::Codex)).unwrap();
        assert!(c.args.windows(2).any(|w| {
            w == [
                "--config".to_string(),
                "model_reasoning_effort=\"ultra\"".to_string(),
            ]
        }));
    }

    #[test]
    fn custom_effort_serializes_as_a_plain_string() {
        let value = serde_json::to_value(Effort::Custom("future".into())).unwrap();
        assert_eq!(value, serde_json::json!("future"));
        assert_eq!(
            serde_json::from_value::<Effort>(value).unwrap(),
            Effort::Custom("future".into())
        );
    }

    #[test]
    fn codex_plan_mode_is_an_explicit_config_override() {
        let s = LaunchSpec {
            permission: Some(Permission::Plan),
            ..spec(Agent::Codex)
        };
        let c = s.resolve(&binary(Agent::Codex)).unwrap();
        assert!(c.args.windows(2).any(|w| {
            w == [
                "--config".to_string(),
                "collaboration_mode.mode=\"Plan\"".to_string(),
            ]
        }));
    }

    #[test]
    fn codex_resume_is_a_subcommand_and_comes_first() {
        let s = LaunchSpec {
            resume: Resume::Last,
            ..spec(Agent::Codex)
        };
        let c = s.resolve(&binary(Agent::Codex)).unwrap();
        assert_eq!(&c.args[..2], ["resume", "--last"]);
    }

    #[test]
    fn claude_fork_keeps_the_original() {
        let s = LaunchSpec {
            resume: Resume::Fork("abc-123".into()),
            ..spec(Agent::Claude)
        };
        let c = s.resolve(&binary(Agent::Claude)).unwrap();
        assert_eq!(c.args, ["--resume", "abc-123", "--fork-session"]);
    }

    #[test]
    fn unsupported_options_are_refused_not_dropped() {
        let s = LaunchSpec {
            sandbox: Some(Sandbox::ReadOnly),
            ..spec(Agent::Claude)
        };
        assert!(matches!(
            s.resolve(&binary(Agent::Claude)),
            Err(LaunchError::Unsupported {
                flag: "--sandbox",
                ..
            })
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
        assert_eq!(
            c.args,
            ["--model", "sonnet", "--", "fix the & in the parser"]
        );
    }

    #[test]
    fn dash_leading_prompt_is_never_parsed_as_an_option() {
        for agent in [Agent::Claude, Agent::Codex] {
            let s = LaunchSpec {
                initial_prompt: Some("--dangerously-skip-permissions".into()),
                extra_args: vec!["--verbose".into()],
                ..spec(agent)
            };
            let args = s.resolve(&binary(agent)).unwrap().args;
            let separator = args.iter().position(|arg| arg == "--").unwrap();
            assert_eq!(args[separator + 1], "--dangerously-skip-permissions");
            assert_eq!(args[separator - 1], "--verbose");
        }
    }

    #[test]
    fn invalid_resume_ids_are_refused_before_argv_is_built() {
        for resume in [
            Resume::Session("--dangerously-skip-permissions".into()),
            Resume::Fork("--config=sandbox_mode=\"danger-full-access\"".into()),
        ] {
            let spec = LaunchSpec {
                resume,
                ..spec(Agent::Claude)
            };
            assert!(matches!(
                spec.resolve(&binary(Agent::Claude)),
                Err(LaunchError::InvalidResumeId)
            ));
        }
    }

    #[test]
    fn missing_cwd_fails_before_spawn() {
        let s = LaunchSpec {
            cwd: PathBuf::from("/nope/nope"),
            ..spec(Agent::Claude)
        };
        assert!(matches!(
            s.resolve(&binary(Agent::Claude)),
            Err(LaunchError::MissingCwd(_))
        ));
    }

    #[test]
    fn budget_is_plain_decimal() {
        assert_eq!(format_usd(5.0), "5");
        assert_eq!(format_usd(0.5), "0.5");
        assert_eq!(format_usd(12.25), "12.25");
    }

    #[test]
    fn invalid_budgets_are_refused_before_spawn() {
        for budget in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let s = LaunchSpec {
                max_budget_usd: Some(budget),
                ..spec(Agent::Claude)
            };
            assert!(matches!(
                s.resolve(&binary(Agent::Claude)),
                Err(LaunchError::InvalidBudget)
            ));
        }
    }

    #[test]
    fn resume_json_uses_an_explicit_id_field_for_targeted_sessions() {
        let session = serde_json::to_value(Resume::Session("abc-123".into())).unwrap();
        let fork = serde_json::to_value(Resume::Fork("def-456".into())).unwrap();
        assert_eq!(
            session,
            serde_json::json!({"kind": "session", "id": "abc-123"})
        );
        assert_eq!(fork, serde_json::json!({"kind": "fork", "id": "def-456"}));
        assert_eq!(
            serde_json::from_value::<Resume>(session).unwrap(),
            Resume::Session("abc-123".into())
        );
    }
}
