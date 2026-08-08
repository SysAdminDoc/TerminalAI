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
use crate::manifest::{AgentManifest, Emit, PermissionMode, PermissionTable, Slot};

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
///
/// Open for the same reason [`Effort`] is: a runtime adds modes faster than
/// this binary ships. Claude Code alone has grown `auto`, `dontAsk` and
/// `manual` since these four were written, and a closed enum does not merely
/// fail to offer them — it silently rewrites a preset or repository template
/// that names one, which is the quiet data loss this project refuses
/// everywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    /// Claude `default` / Codex `on-request`.
    Ask,
    /// Claude `plan` — read and propose, never edit. No Codex equivalent, so
    /// Codex sessions pair this with the read-only sandbox.
    Plan,
    /// Claude `acceptEdits` / Codex `on-request` paired with the
    /// workspace-write sandbox, which is Codex's own auto preset. Deliberately
    /// *not* Codex `untrusted`: that policy runs only known-safe reads without
    /// asking, so it prompts more than [`Permission::Ask`] does rather than
    /// less, and mapping to it inverted the whole ladder.
    AcceptEdits,
    /// Claude `bypassPermissions` / Codex `never`.
    Bypass,
    /// A mode this binary does not model, passed to the chosen agent verbatim
    /// with a warning. The value is the agent's own spelling, so it is not
    /// portable between agents — the same string is handed to
    /// `--permission-mode` or `--ask-for-approval` as chosen.
    Custom(String),
}

impl Permission {
    /// The wire and config spelling. The four modelled modes are kebab-case so
    /// stored presets and repository templates keep the names they always had.
    pub fn as_str(&self) -> &str {
        match self {
            Permission::Ask => "ask",
            Permission::Plan => "plan",
            Permission::AcceptEdits => "accept-edits",
            Permission::Bypass => "bypass",
            Permission::Custom(value) => value,
        }
    }

    /// Parse a stored or operator-supplied value, keeping anything unmodelled
    /// rather than discarding it.
    pub fn parse(value: &str) -> Self {
        match value {
            "ask" => Permission::Ask,
            "plan" => Permission::Plan,
            "accept-edits" => Permission::AcceptEdits,
            "bypass" => Permission::Bypass,
            _ => Permission::Custom(value.to_owned()),
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom(_))
    }
}

impl Serialize for Permission {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Permission {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Permission::parse(&value))
    }
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
    /// Where the agent keeps its own configuration and signed-in session:
    /// `CLAUDE_CONFIG_DIR` for Claude Code, `CODEX_HOME` for Codex. Two sessions
    /// pointed at different directories are two accounts.
    #[serde(default)]
    pub agent_home: Option<PathBuf>,
    /// Parent variables this launch may inherit, named one at a time.
    ///
    /// The baseline allowlist carries no credential of any kind, which is
    /// correct as a default and leaves API-key, Bedrock and Vertex operators
    /// with an agent that cannot authenticate — a failure that surfaces as an
    /// expired login rather than as an unsupported configuration. Naming a
    /// variable here is the operator's explicit consent for this session; a
    /// variable absent from the parent is a refusal, not a silent omission.
    #[serde(default)]
    pub env_passthrough: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub permission: Option<Permission>,
    /// Codex only.
    pub sandbox: Option<Sandbox>,
    /// Codex only — layers `$CODEX_HOME/<name>.config.toml`.
    pub profile: Option<String>,
    pub add_dirs: Vec<PathBuf>,
    /// Provider-native id for a new session. Claude accepts this as
    /// `--session-id`, which lets transcript tailing bind by name instead of
    /// guessing from filesystem timestamps. Older stored specs leave it empty.
    #[serde(default)]
    pub session_id: Option<String>,
    pub resume: Resume,
    /// A per-session spend cap, enforced by this tool's own ledger.
    ///
    /// Deliberately *not* an argv flag. Claude Code's `--max-budget-usd`
    /// documents itself as "only works with --print", and every session this
    /// tool supervises is interactive — emitting it would render a control that
    /// implies a cap and binds nothing, which is the one failure mode a spend
    /// feature must not have. The cap is applied against the transcript-derived
    /// cost in `registry::sampling`, which reads both agents' transcripts, so it
    /// is agent-independent rather than Claude-only.
    pub max_budget_usd: Option<f64>,
    /// Codex only.
    pub web_search: bool,
    /// How many subagents this session may run at once.
    ///
    /// Admission governs sessions, spend and memory — the things one row costs.
    /// It has no view of the one multiplier a *single* session controls: since
    /// Claude Code 2.1.216 a session runs up to twenty concurrent subagents by
    /// default, and with agent teams it can also hold several separate agent
    /// instances. Delivered as an environment variable because that is the only
    /// interface the agent offers for it; there is no flag.
    ///
    /// `None` leaves the agent's own default in place. Zero is refused rather
    /// than clamped, for the same reason the admission gate refuses a zero
    /// session cap: silently turning "none" into "one" gives the operator the
    /// opposite of what they asked for.
    #[serde(default)]
    pub max_concurrent_subagents: Option<u32>,
    /// Whether this session may start an agent team.
    ///
    /// `None` inherits whatever the agent's own configuration says; `Some` states
    /// it either way, because "teams off" is a decision an operator makes about
    /// a session's cost and it should not depend on ambient configuration.
    #[serde(default)]
    pub agent_teams: Option<bool>,
    /// Tool names this session may use without asking, and those it may not use
    /// at all. Permission-prompt fatigue is the loudest complaint about these
    /// agents, and this is the precise lever for it: an allowlist answers the
    /// prompts in advance instead of turning them off wholesale the way bypass
    /// mode does.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    /// A settings file or JSON string layered on top of the agent's own.
    #[serde(default)]
    pub settings: Option<String>,
    /// Which setting sources to load, as the agent spells them.
    #[serde(default)]
    pub setting_sources: Option<String>,
    /// MCP server definitions for this session only.
    #[serde(default)]
    pub mcp_config: Vec<String>,
    /// Use only the MCP servers named above, ignoring every other source.
    #[serde(default)]
    pub strict_mcp_config: bool,
    /// Plugins loaded for this session only, by directory and by URL. A URL
    /// fetches remote code, so it is an operator decision and never a
    /// repository's — [`crate::template`] cannot reach any of these fields.
    #[serde(default)]
    pub plugin_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub plugin_urls: Vec<String>,
    /// Model to fall back to when the primary is overloaded.
    #[serde(default)]
    pub fallback_model: Option<String>,
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
    #[error("{0} cannot resume a session from the command line; use New")]
    ResumeUnsupported(&'static str),
    #[error("resume session id has an invalid command-line shape")]
    InvalidResumeId,
    #[error("max_budget_usd must be finite and non-negative")]
    InvalidBudget,
    #[error("the concurrent subagent cap must be at least 1; leave it unset for the agent's default")]
    InvalidSubagentCap,
    #[error("accept-edits cannot be combined with the read-only sandbox: the agent would accept edits it is not permitted to make")]
    AcceptEditsUnderReadOnlySandbox,
    #[error("environment variable name {0:?} is not a plain name")]
    InvalidEnvironmentName(String),
    #[error("{0} is set by the supervisor and cannot be inherited from the parent")]
    ReservedEnvironmentName(String),
    #[error("{0} is not set in this process, so there is nothing to pass through")]
    UnsetEnvironmentName(String),
    #[error("agent home directory does not exist: {0}")]
    MissingAgentHome(PathBuf),
    #[error("{field} value {value:?} would be read as an option, not as a value")]
    OptionShapedValue {
        field: &'static str,
        value: String,
    },
    #[error("plugin URL {0:?} is not an http(s) URL")]
    InvalidPluginUrl(String),
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
        if self
            .session_id
            .as_deref()
            .is_some_and(|id| !is_valid_resume_id(id))
        {
            return Err(LaunchError::InvalidResumeId);
        }
        if self.permission == Some(Permission::AcceptEdits)
            && self.sandbox == Some(Sandbox::ReadOnly)
        {
            return Err(LaunchError::AcceptEditsUnderReadOnlySandbox);
        }
        self.validate_passthrough()?;
        self.environment.validate()?;
        // Validated here so a bad passthrough name refuses the launch rather
        // than surfacing later as an agent that cannot authenticate.
        self.agent_environment()?;
        let args = self.args_from(self.agent.manifest())?;
        Ok(ResolvedCommand {
            program: binary.path.clone(),
            args,
            cwd: self.cwd.clone(),
        })
    }

    /// Refuse a passthrough value that would be parsed as an option.
    ///
    /// These fields become command-line tokens next to flags that decide what
    /// the agent may do, so a value beginning with `-` is not a value at all —
    /// it is a second option the operator did not choose, in a position where
    /// `--dangerously-skip-permissions` would be accepted. `extra_args` is
    /// deliberately exempt: it is documented as the trusted-input-only escape
    /// hatch and exists precisely to pass flags.
    ///
    /// A plugin URL is checked further, because it fetches and runs remote code:
    /// a `file:` or `data:` URL there would be a different mechanism wearing the
    /// same field's name.
    fn validate_passthrough(&self) -> Result<(), LaunchError> {
        let lists: [(&'static str, &[String]); 4] = [
            ("allowed_tools", &self.allowed_tools),
            ("disallowed_tools", &self.disallowed_tools),
            ("mcp_config", &self.mcp_config),
            ("plugin_urls", &self.plugin_urls),
        ];
        for (field, values) in lists {
            for value in values {
                refuse_option_shaped(field, value)?;
            }
        }
        for (field, value) in [
            ("settings", self.settings.as_deref()),
            ("setting_sources", self.setting_sources.as_deref()),
            ("fallback_model", self.fallback_model.as_deref()),
        ] {
            if let Some(value) = value {
                refuse_option_shaped(field, value)?;
            }
        }
        for directory in &self.plugin_dirs {
            refuse_option_shaped("plugin_dirs", &directory.to_string_lossy())?;
        }
        for url in &self.plugin_urls {
            let lowered = url.to_ascii_lowercase();
            if !lowered.starts_with("https://") && !lowered.starts_with("http://") {
                return Err(LaunchError::InvalidPluginUrl(url.clone()));
            }
        }
        Ok(())
    }

    /// The variables this launch adds on top of the sanitized baseline.
    ///
    /// Every one is here because the spec asked for it. Nothing is read from the
    /// parent unless named, and a name that is reserved, malformed or unset is
    /// refused rather than dropped — an operator who asked to pass a credential
    /// through and silently got a session without it would debug the agent.
    pub fn agent_environment(&self) -> Result<Vec<(String, String)>, LaunchError> {
        let mut pairs = Vec::new();
        // Refused, not dropped -- the same rule the argv slots follow. An
        // operator who capped a session's fan-out and silently got an uncapped
        // one has been told something untrue about what they launched.
        if let Some(cap) = self.max_concurrent_subagents {
            if self.agent != Agent::Claude {
                return Err(LaunchError::Unsupported {
                    flag: MAX_CONCURRENT_SUBAGENTS,
                    agent: self.agent.label(),
                });
            }
            if cap == 0 {
                return Err(LaunchError::InvalidSubagentCap);
            }
            pairs.push((MAX_CONCURRENT_SUBAGENTS.to_owned(), cap.to_string()));
        }
        if let Some(teams) = self.agent_teams {
            if self.agent != Agent::Claude {
                return Err(LaunchError::Unsupported {
                    flag: AGENT_TEAMS,
                    agent: self.agent.label(),
                });
            }
            pairs.push((
                AGENT_TEAMS.to_owned(),
                if teams { "1" } else { "0" }.to_owned(),
            ));
        }
        if let Some(home) = &self.agent_home {
            if !home.is_dir() {
                return Err(LaunchError::MissingAgentHome(home.clone()));
            }
            pairs.push((
                self.agent.home_env().to_owned(),
                home.to_string_lossy().into_owned(),
            ));
        }
        for name in &self.env_passthrough {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                || name.chars().next().is_some_and(|c| c.is_ascii_digit())
            {
                return Err(LaunchError::InvalidEnvironmentName(name.clone()));
            }
            // The supervisor's own variables carry the per-session hook secret
            // and the identity a hook is trusted by. A parent value overwriting
            // one would let an unrelated process's environment rebind this row.
            if name.starts_with("TERMINALAI_") {
                return Err(LaunchError::ReservedEnvironmentName(name.clone()));
            }
            let Some(value) = std::env::var_os(name) else {
                return Err(LaunchError::UnsetEnvironmentName(name.clone()));
            };
            pairs.push((name.clone(), value.to_string_lossy().into_owned()));
        }
        Ok(pairs)
    }

    /// Build the argument vector from a manifest.
    ///
    /// The *shape* is data — `order` decides which slots appear and where, and
    /// `flags` decides how each is spelled. What stays here is the logic that is
    /// not spelling: which fields refuse rather than drop, that a new session id
    /// is only passed on a new session, and that the prompt is positional and
    /// last behind a `--` separator so a dash-leading prompt can never be parsed
    /// as an option.
    fn args_from(&self, manifest: &AgentManifest) -> Result<Vec<String>, LaunchError> {
        self.refuse_unexpressed(manifest)?;
        let flags = &manifest.flags;
        // Resolved before the loop because a permission mode can imply a
        // sandbox, and the two slots need not be adjacent in `order`.
        let sandbox = self.effective_sandbox(manifest);
        let mut a: Vec<String> = Vec::new();
        for slot in &manifest.order {
            match slot {
                Slot::Model => emit_optional(&mut a, &flags.model, self.model.as_deref()),
                Slot::Effort => emit_optional(
                    &mut a,
                    &flags.effort,
                    self.effort.as_ref().map(Effort::as_str),
                ),
                Slot::Permission => self.emit_permission(&mut a, manifest)?,
                Slot::Sandbox => emit_optional(&mut a, &flags.sandbox, sandbox.map(Sandbox::as_str)),
                Slot::Profile => emit_optional(&mut a, &flags.profile, self.profile.as_deref()),
                Slot::Name => emit_optional(&mut a, &flags.name, self.name.as_deref()),
                Slot::SessionId => {
                    // A resume already names the session; repeating the id as a
                    // new-session id would ask the agent for two identities.
                    if matches!(self.resume, Resume::New) {
                        emit_optional(&mut a, &flags.session_id, self.session_id.as_deref());
                    }
                }
                Slot::Cwd => {
                    emit_optional(&mut a, &flags.cwd, Some(&self.cwd.to_string_lossy()));
                }
                Slot::AddDirs => {
                    if let Some(emit) = &flags.add_dirs {
                        for directory in &self.add_dirs {
                            emit.push(&mut a, &directory.to_string_lossy());
                        }
                    }
                }
                Slot::WebSearch => emit_switch(&mut a, &flags.web_search, self.web_search),
                Slot::AllowedTools => {
                    emit_each(&mut a, &flags.allowed_tools, &self.allowed_tools)
                }
                Slot::DisallowedTools => {
                    emit_each(&mut a, &flags.disallowed_tools, &self.disallowed_tools)
                }
                Slot::Settings => emit_optional(&mut a, &flags.settings, self.settings.as_deref()),
                Slot::SettingSources => {
                    emit_optional(&mut a, &flags.setting_sources, self.setting_sources.as_deref())
                }
                Slot::McpConfig => emit_each(&mut a, &flags.mcp_config, &self.mcp_config),
                Slot::StrictMcpConfig => {
                    emit_switch(&mut a, &flags.strict_mcp_config, self.strict_mcp_config)
                }
                Slot::PluginDirs => {
                    if let Some(emit) = &flags.plugin_dirs {
                        for directory in &self.plugin_dirs {
                            emit.push(&mut a, &directory.to_string_lossy());
                        }
                    }
                }
                Slot::PluginUrls => emit_each(&mut a, &flags.plugin_urls, &self.plugin_urls),
                Slot::FallbackModel => {
                    emit_optional(&mut a, &flags.fallback_model, self.fallback_model.as_deref())
                }
                Slot::Resume => self.emit_resume(&mut a, manifest)?,
            }
        }
        a.extend(self.extra_args.iter().cloned());
        if let Some(prompt) = &self.initial_prompt {
            a.push("--".into());
            a.push(prompt.clone());
        }
        Ok(a)
    }

    /// Refuse a choice the chosen agent cannot express.
    ///
    /// Only the slots that carry a deliberate operator decision are here. `name`,
    /// `session-id` and `cwd` are supervisor-side conveniences an agent may
    /// legitimately not need — Codex names its rows here rather than in the CLI,
    /// and Claude inherits its working directory from the process — so their
    /// absence from a manifest is a fact about the agent, not a dropped choice.
    fn refuse_unexpressed(&self, manifest: &AgentManifest) -> Result<(), LaunchError> {
        let set = [
            (Slot::Model, self.model.is_some()),
            (Slot::Effort, self.effort.is_some()),
            (Slot::Permission, self.permission.is_some()),
            (Slot::Sandbox, self.sandbox.is_some()),
            (Slot::Profile, self.profile.is_some()),
            (Slot::AddDirs, !self.add_dirs.is_empty()),
            (Slot::WebSearch, self.web_search),
            (Slot::AllowedTools, !self.allowed_tools.is_empty()),
            (Slot::DisallowedTools, !self.disallowed_tools.is_empty()),
            (Slot::Settings, self.settings.is_some()),
            (Slot::SettingSources, self.setting_sources.is_some()),
            (Slot::McpConfig, !self.mcp_config.is_empty()),
            (Slot::StrictMcpConfig, self.strict_mcp_config),
            (Slot::PluginDirs, !self.plugin_dirs.is_empty()),
            (Slot::PluginUrls, !self.plugin_urls.is_empty()),
            (Slot::FallbackModel, self.fallback_model.is_some()),
        ];
        for (slot, chosen) in set {
            if chosen && !manifest.expresses(slot) {
                return Err(LaunchError::Unsupported {
                    flag: slot.option_name(),
                    agent: self.agent.label(),
                });
            }
        }
        Ok(())
    }

    /// The sandbox this launch actually runs under.
    ///
    /// A permission mode may imply one: Codex expresses auto-editing as
    /// `on-request` *paired with* workspace-write, and its default sandbox is
    /// not guaranteed to permit writes, so accept-edits without a sandbox would
    /// ask the agent to accept edits it cannot make. The contradictory
    /// combination — accept-edits with an explicit read-only sandbox — is
    /// refused in [`LaunchSpec::resolve`], so an implied value is always one
    /// that can write.
    fn effective_sandbox(&self, manifest: &AgentManifest) -> Option<Sandbox> {
        if self.sandbox.is_some() {
            return self.sandbox;
        }
        let table = manifest.flags.permission.as_ref()?;
        let mode = permission_mode(table, self.permission.as_ref()?)?;
        mode.implies_sandbox
    }

    fn emit_permission(
        &self,
        args: &mut Vec<String>,
        manifest: &AgentManifest,
    ) -> Result<(), LaunchError> {
        let Some(permission) = self.permission.as_ref() else {
            return Ok(());
        };
        let Some(table) = manifest.flags.permission.as_ref() else {
            return Ok(());
        };
        match permission_mode(table, permission) {
            Some(mode) => {
                let emit = mode.emit.as_ref().unwrap_or(&table.emit);
                emit.push(args, &mode.value);
            }
            // A mode this build models but this agent does not spell is a
            // refusal, not a silent downgrade to whatever the agent defaults to.
            None if !permission.is_custom() => {
                return Err(LaunchError::Unsupported {
                    flag: Slot::Permission.option_name(),
                    agent: self.agent.label(),
                });
            }
            // The agent's own spelling for a mode this build does not model —
            // Claude's `auto`/`dontAsk`/`manual`, Codex's granular
            // `approval_policy` values. Passed through verbatim.
            None => table.emit.push(args, permission.as_str()),
        }
        Ok(())
    }

    fn emit_resume(
        &self,
        args: &mut Vec<String>,
        manifest: &AgentManifest,
    ) -> Result<(), LaunchError> {
        let (id, tokens) = match (&self.resume, manifest.flags.resume.as_ref()) {
            (Resume::New, _) | (_, None) => return Ok(()),
            (Resume::Last, Some(table)) => (None, &table.last),
            (Resume::Session(id), Some(table)) => (Some(id), &table.session),
            (Resume::Fork(id), Some(table)) => (Some(id), &table.fork),
        };
        let Some(tokens) = tokens else {
            return Err(LaunchError::ResumeUnsupported(self.agent.label()));
        };
        for token in tokens {
            args.push(match id {
                Some(id) => token.replace("{id}", id),
                None => token.clone(),
            });
        }
        Ok(())
    }
}

/// Emit `value` through `emit` when both are present.
fn emit_optional(args: &mut Vec<String>, emit: &Option<Emit>, value: Option<&str>) {
    if let (Some(emit), Some(value)) = (emit, value) {
        emit.push(args, value);
    }
}

fn refuse_option_shaped(field: &'static str, value: &str) -> Result<(), LaunchError> {
    if value.starts_with('-') {
        return Err(LaunchError::OptionShapedValue {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Emit one flag occurrence per value.
///
/// Repeating the flag rather than joining the values is deliberate: both CLIs
/// document these options as repeatable or space-separated, and a joined value
/// would be re-split on a separator that appears inside a tool pattern —
/// `Bash(git log:*)` contains a space, and a plugin path can contain one too.
fn emit_each(args: &mut Vec<String>, emit: &Option<Emit>, values: &[String]) {
    if let Some(emit) = emit {
        for value in values {
            emit.push(args, value);
        }
    }
}

/// Emit a valueless switch when the choice was made.
fn emit_switch(args: &mut Vec<String>, emit: &Option<Emit>, chosen: bool) {
    if let (Some(emit), true) = (emit, chosen) {
        emit.push_switch(args);
    }
}

/// The manifest entry for a modelled permission mode. `None` for a custom mode,
/// and for a modelled mode this agent does not spell.
fn permission_mode<'a>(
    table: &'a PermissionTable,
    permission: &Permission,
) -> Option<&'a PermissionMode> {
    match permission {
        Permission::Ask => table.ask.as_ref(),
        Permission::Plan => table.plan.as_ref(),
        Permission::AcceptEdits => table.accept_edits.as_ref(),
        Permission::Bypass => table.bypass.as_ref(),
        Permission::Custom(_) => None,
    }
}

/// `--max-budget-usd` wants a plain decimal, not scientific notation.
/// Claude Code's own cap on how many subagents one session runs at once.
/// Default 20 as of 2.1.216; there is no flag for it.
pub const MAX_CONCURRENT_SUBAGENTS: &str = "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS";

/// Whether a session may start an agent team, where each teammate is a separate
/// agent instance. Experimental and opt-in.
pub const AGENT_TEAMS: &str = "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS";

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
                "api rewrite"
            ]
        );
    }

    #[test]
    fn claude_new_session_id_is_passed_but_resume_ids_are_not_repeated() {
        let s = LaunchSpec {
            session_id: Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into()),
            ..spec(Agent::Claude)
        };
        let c = s.resolve(&binary(Agent::Claude)).unwrap();
        assert_eq!(c.args, [
            "--session-id",
            "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
        ]);

        let s = LaunchSpec {
            session_id: Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into()),
            resume: Resume::Session("old-session".into()),
            ..spec(Agent::Claude)
        };
        let c = s.resolve(&binary(Agent::Claude)).unwrap();
        assert_eq!(c.args, ["--resume", "old-session"]);
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
    fn a_session_can_bound_its_own_fan_out() {
        // The one resource multiplier admission cannot see: it governs how many
        // sessions run, not how many agents one session is.
        let spec = LaunchSpec {
            max_concurrent_subagents: Some(4),
            agent_teams: Some(true),
            ..spec(Agent::Claude)
        };
        let environment = spec.agent_environment().expect("Claude expresses both");
        assert!(environment.contains(&(MAX_CONCURRENT_SUBAGENTS.to_owned(), "4".to_owned())));
        assert!(environment.contains(&(AGENT_TEAMS.to_owned(), "1".to_owned())));
    }

    #[test]
    fn teams_are_refused_explicitly_rather_than_left_to_ambient_configuration() {
        // "Off" has to be a value this launch sets. Leaving it unset means
        // whatever the machine's configuration happens to say, which is not the
        // same decision and is not visible on the row.
        let off = LaunchSpec {
            agent_teams: Some(false),
            ..spec(Agent::Claude)
        };
        assert!(off
            .agent_environment()
            .expect("valid")
            .contains(&(AGENT_TEAMS.to_owned(), "0".to_owned())));
        let unset = spec(Agent::Claude);
        assert!(!unset
            .agent_environment()
            .expect("valid")
            .iter()
            .any(|(name, _)| name == AGENT_TEAMS));
    }

    #[test]
    fn codex_is_refused_rather_than_launched_as_if_it_had_a_fan_out_cap() {
        for spec in [
            LaunchSpec {
                max_concurrent_subagents: Some(4),
                ..spec(Agent::Codex)
            },
            LaunchSpec {
                agent_teams: Some(true),
                ..spec(Agent::Codex)
            },
        ] {
            assert!(
                matches!(
                    spec.agent_environment(),
                    Err(LaunchError::Unsupported { .. })
                ),
                "Codex accepted a cap it has no equivalent for"
            );
        }
    }

    #[test]
    fn a_cap_of_zero_is_refused_rather_than_read_as_no_cap() {
        // Zero and unset are opposite requests. `None` means the agent's own
        // default of twenty; zero would have to mean no subagents at all, and
        // the variable has no spelling for that.
        let spec = LaunchSpec {
            max_concurrent_subagents: Some(0),
            ..spec(Agent::Claude)
        };
        assert!(matches!(
            spec.agent_environment(),
            Err(LaunchError::InvalidSubagentCap)
        ));
    }

    #[test]
    fn a_budget_never_reaches_the_argv_of_an_interactive_session() {
        // `claude --help` on 2.1.170: "--max-budget-usd <amount>  Maximum dollar
        // amount to spend on API calls (only works with --print)". Every session
        // this tool supervises is interactive, so the flag would be accepted and
        // ignored. The cap is kept as a field and enforced by the ledger; what
        // must never happen again is it being spelled into a command line.
        for agent in [Agent::Claude, Agent::Codex] {
            let s = LaunchSpec {
                max_budget_usd: Some(5.0),
                ..spec(agent)
            };
            let c = s
                .resolve(&binary(agent))
                .unwrap_or_else(|error| panic!("{agent:?} accepts a ledger budget: {error}"));
            assert!(
                !c.args.iter().any(|arg| arg.contains("budget")),
                "{agent:?} emitted {:?}",
                c.args
            );
        }
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

    /// Parse one family out of manifest text, so a test can state the mapping
    /// it is asserting on rather than depending on the built-in.
    fn manifest_from(text: &str) -> AgentManifest {
        crate::manifest::parse(text)
            .expect("test manifest parses")
            .pop()
            .expect("test manifest declares a family")
    }

    #[test]
    fn the_manifest_decides_the_shape_of_the_argument_vector() {
        // Same spec, a different manifest: the order list moves the model to the
        // end and the flag table renames it. Nothing in Rust changed.
        let manifest = manifest_from(
            r#"
            [[agent]]
            family = "claude"
            label = "Claude Code"
            command-name = "claude"
            exe-stem = "claude"
            version-marker = "claude code"
            home-env = "CLAUDE_CONFIG_DIR"
            probe = "claude-print-json"
            order = ["name", "model"]
            [agent.flags]
            name = { flag = "--name" }
            model = { flag = "--config", config-key = "model" }
            "#,
        );
        let spec = LaunchSpec {
            model: Some("opus".into()),
            name: Some("row".into()),
            ..spec(Agent::Claude)
        };
        assert_eq!(
            spec.args_from(&manifest).unwrap(),
            ["--name", "row", "--config", "model=\"opus\""]
        );
    }

    #[test]
    fn a_choice_the_manifest_does_not_express_is_refused_not_dropped() {
        let manifest = manifest_from(
            r#"
            [[agent]]
            family = "codex"
            label = "Codex"
            command-name = "codex"
            exe-stem = "codex"
            version-marker = "codex"
            home-env = "CODEX_HOME"
            probe = "codex-app-server"
            order = ["model"]
            [agent.flags]
            model = { flag = "--model" }
            "#,
        );
        let spec = LaunchSpec {
            profile: Some("work".into()),
            ..spec(Agent::Codex)
        };
        assert!(matches!(
            spec.args_from(&manifest),
            Err(LaunchError::Unsupported {
                flag: "--profile",
                ..
            })
        ));
    }

    #[test]
    fn a_permission_mode_the_manifest_omits_is_refused_rather_than_downgraded() {
        // The dangerous silent failure this replaces: an agent that cannot
        // express "plan" starting in whatever mode it defaults to.
        let manifest = manifest_from(
            r#"
            [[agent]]
            family = "codex"
            label = "Codex"
            command-name = "codex"
            exe-stem = "codex"
            version-marker = "codex"
            home-env = "CODEX_HOME"
            probe = "codex-app-server"
            order = ["permission"]
            [agent.flags.permission]
            emit = { flag = "--ask-for-approval" }
            ask = { value = "on-request" }
            "#,
        );
        let planning = LaunchSpec {
            permission: Some(Permission::Plan),
            ..spec(Agent::Codex)
        };
        assert!(matches!(
            planning.args_from(&manifest),
            Err(LaunchError::Unsupported {
                flag: "--permission-mode",
                ..
            })
        ));
        // A mode this build does not model still passes through: the manifest
        // says nothing about it either way, and the agent is the authority.
        let unmodelled = LaunchSpec {
            permission: Some(Permission::Custom("untrusted".into())),
            ..spec(Agent::Codex)
        };
        assert_eq!(
            unmodelled.args_from(&manifest).unwrap(),
            ["--ask-for-approval", "untrusted"]
        );
    }

    #[test]
    fn a_manifest_with_no_resume_tokens_refuses_a_resume_rather_than_starting_fresh() {
        let manifest = manifest_from(
            r#"
            [[agent]]
            family = "claude"
            label = "Claude Code"
            command-name = "claude"
            exe-stem = "claude"
            version-marker = "claude code"
            home-env = "CLAUDE_CONFIG_DIR"
            probe = "claude-print-json"
            order = ["resume"]
            [agent.flags.resume]
            last = ["--continue"]
            "#,
        );
        let forking = LaunchSpec {
            resume: Resume::Fork("abc-123".into()),
            ..spec(Agent::Claude)
        };
        assert!(matches!(
            forking.args_from(&manifest),
            Err(LaunchError::ResumeUnsupported(_))
        ));
        let continuing = LaunchSpec {
            resume: Resume::Last,
            ..spec(Agent::Claude)
        };
        assert_eq!(continuing.args_from(&manifest).unwrap(), ["--continue"]);
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
