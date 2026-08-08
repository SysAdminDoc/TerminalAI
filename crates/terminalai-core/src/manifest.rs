//! Agent identity and flag mapping as data.
//!
//! Everything that differs between one supervised agent family and another used
//! to be a match arm: the label, the executable name, the version banner that
//! identifies it, where npm puts the native binary, and — the expensive half —
//! a hand-written per-family function turning launcher choices into an argument
//! vector. Adding a third family meant editing a Rust enum, two flag tables and
//! the capability probe, and that cost, not the transport, was what kept a
//! third family off the roadmap.
//!
//! So the differences live in `agents/builtin.toml`, embedded at build time and
//! parsed once. A family is described by:
//!
//! * identity — label, command name, executable stem, version marker, the npm
//!   package path and the environment variable naming its own config directory;
//! * `order` — the slots this family emits, in argument-vector order;
//! * `flags` — how each slot is spelled.
//!
//! The two lists are checked against each other at load: a slot in `order` with
//! no flag entry, or a flag entry no `order` mentions, is refused with the field
//! named. That is the same spirit as `.terminalai/templates.toml`'s
//! `deny_unknown_fields` — a configuration file that asks for something the
//! model does not express is an error, never a silently dropped field.
//!
//! What is deliberately *not* data: the checks that are about safety rather than
//! spelling (accept-edits under a read-only sandbox, resume id shapes, budget
//! finiteness) stay in [`crate::launch`], and the capability probe protocols
//! stay in [`crate::capabilities`] — the manifest names which protocol a family
//! speaks, it does not describe it.
//!
//! Manifests are trusted local configuration. [`load_overrides`] reads one from
//! the operator's own data directory. A repository may not supply one: a
//! repo-declared agent definition is arbitrary argv arriving with a clone.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::agent::Agent;
use crate::launch::Sandbox;

/// The manifests this binary ships with.
const BUILTIN: &str = include_str!("../agents/builtin.toml");

/// Where an operator may override the built-ins, under their data directory.
pub const OVERRIDE_FILE: &str = "agents.toml";

/// One argument-vector slot. The name is the spelling used in a manifest's
/// `order` list and in its `flags` table, so the two cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    Model,
    Effort,
    Permission,
    Sandbox,
    Profile,
    Name,
    SessionId,
    Cwd,
    AddDirs,
    WebSearch,
    AllowedTools,
    DisallowedTools,
    Settings,
    SettingSources,
    McpConfig,
    StrictMcpConfig,
    PluginDirs,
    PluginUrls,
    FallbackModel,
    Resume,
}

impl Slot {
    /// The option name reported when a family cannot express this slot. It is
    /// the flag an operator would look for, not this build's field name.
    pub fn option_name(self) -> &'static str {
        match self {
            Slot::Model => "--model",
            Slot::Effort => "--effort",
            Slot::Permission => "--permission-mode",
            Slot::Sandbox => "--sandbox",
            Slot::Profile => "--profile",
            Slot::Name => "--name",
            Slot::SessionId => "--session-id",
            Slot::Cwd => "--cd",
            Slot::AddDirs => "--add-dir",
            Slot::WebSearch => "--search",
            Slot::AllowedTools => "--allowed-tools",
            Slot::DisallowedTools => "--disallowed-tools",
            Slot::Settings => "--settings",
            Slot::SettingSources => "--setting-sources",
            Slot::McpConfig => "--mcp-config",
            Slot::StrictMcpConfig => "--strict-mcp-config",
            Slot::PluginDirs => "--plugin-dir",
            Slot::PluginUrls => "--plugin-url",
            Slot::FallbackModel => "--fallback-model",
            Slot::Resume => "--resume",
        }
    }

    /// The manifest spelling, used in error messages so a refusal names the key
    /// the operator actually typed.
    pub fn key(self) -> &'static str {
        match self {
            Slot::Model => "model",
            Slot::Effort => "effort",
            Slot::Permission => "permission",
            Slot::Sandbox => "sandbox",
            Slot::Profile => "profile",
            Slot::Name => "name",
            Slot::SessionId => "session-id",
            Slot::Cwd => "cwd",
            Slot::AddDirs => "add-dirs",
            Slot::WebSearch => "web-search",
            Slot::AllowedTools => "allowed-tools",
            Slot::DisallowedTools => "disallowed-tools",
            Slot::Settings => "settings",
            Slot::SettingSources => "setting-sources",
            Slot::McpConfig => "mcp-config",
            Slot::StrictMcpConfig => "strict-mcp-config",
            Slot::PluginDirs => "plugin-dirs",
            Slot::PluginUrls => "plugin-urls",
            Slot::FallbackModel => "fallback-model",
            Slot::Resume => "resume",
        }
    }
}

/// How one value reaches the command line.
///
/// Two shapes cover both supported families: an ordinary `--flag value` pair,
/// and a TOML config override — Codex expresses reasoning effort and its
/// collaboration mode as `--config key="value"` rather than as flags of their
/// own. A `switch` carries no value at all.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Emit {
    pub flag: String,
    /// When set, the value is emitted as `key="value"` after the flag.
    #[serde(default)]
    pub config_key: Option<String>,
    /// The flag stands alone; the value is the fact that it was set.
    #[serde(default)]
    pub switch: bool,
}

impl Emit {
    /// Append this emission carrying `value`.
    pub fn push(&self, args: &mut Vec<String>, value: &str) {
        args.push(self.flag.clone());
        match &self.config_key {
            Some(key) => args.push(format!("{key}=\"{value}\"")),
            None => args.push(value.to_owned()),
        }
    }

    /// Append a valueless switch.
    pub fn push_switch(&self, args: &mut Vec<String>) {
        args.push(self.flag.clone());
    }
}

/// One permission mode's spelling for a family.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PermissionMode {
    /// The value handed to the option.
    pub value: String,
    /// Emit through a different option than the table's default — Codex's plan
    /// mode is a config override while its other modes are a flag.
    #[serde(default)]
    pub emit: Option<Emit>,
    /// The sandbox this mode implies when the spec names none. Codex's
    /// accept-edits analogue is `on-request` *paired with* workspace-write;
    /// without the pair the agent would accept edits it cannot make.
    #[serde(default)]
    pub implies_sandbox: Option<Sandbox>,
}

/// How a family spells the permission ladder. A mode absent here is one this
/// family cannot express, and is refused rather than dropped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PermissionTable {
    /// The option every mode uses unless it overrides it. Also the option an
    /// unmodelled (custom) mode is passed through verbatim on.
    pub emit: Emit,
    #[serde(default)]
    pub ask: Option<PermissionMode>,
    #[serde(default)]
    pub plan: Option<PermissionMode>,
    #[serde(default)]
    pub accept_edits: Option<PermissionMode>,
    #[serde(default)]
    pub bypass: Option<PermissionMode>,
}

/// How a family spells resumption. Tokens containing `{id}` are substituted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ResumeTable {
    /// Continue the most recent session in this folder.
    #[serde(default)]
    pub last: Option<Vec<String>>,
    /// Resume a specific session id.
    #[serde(default)]
    pub session: Option<Vec<String>>,
    /// Branch off a session, leaving the original intact.
    #[serde(default)]
    pub fork: Option<Vec<String>>,
}

/// Every slot's spelling for one family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FlagTable {
    #[serde(default)]
    pub model: Option<Emit>,
    #[serde(default)]
    pub effort: Option<Emit>,
    #[serde(default)]
    pub permission: Option<PermissionTable>,
    #[serde(default)]
    pub sandbox: Option<Emit>,
    #[serde(default)]
    pub profile: Option<Emit>,
    #[serde(default)]
    pub name: Option<Emit>,
    #[serde(default)]
    pub session_id: Option<Emit>,
    #[serde(default)]
    pub cwd: Option<Emit>,
    #[serde(default)]
    pub add_dirs: Option<Emit>,
    #[serde(default)]
    pub web_search: Option<Emit>,
    #[serde(default)]
    pub allowed_tools: Option<Emit>,
    #[serde(default)]
    pub disallowed_tools: Option<Emit>,
    #[serde(default)]
    pub settings: Option<Emit>,
    #[serde(default)]
    pub setting_sources: Option<Emit>,
    #[serde(default)]
    pub mcp_config: Option<Emit>,
    #[serde(default)]
    pub strict_mcp_config: Option<Emit>,
    #[serde(default)]
    pub plugin_dirs: Option<Emit>,
    #[serde(default)]
    pub plugin_urls: Option<Emit>,
    #[serde(default)]
    pub fallback_model: Option<Emit>,
    #[serde(default)]
    pub resume: Option<ResumeTable>,
}

impl FlagTable {
    /// True when this family expresses `slot` at all.
    pub fn expresses(&self, slot: Slot) -> bool {
        match slot {
            Slot::Model => self.model.is_some(),
            Slot::Effort => self.effort.is_some(),
            Slot::Permission => self.permission.is_some(),
            Slot::Sandbox => self.sandbox.is_some(),
            Slot::Profile => self.profile.is_some(),
            Slot::Name => self.name.is_some(),
            Slot::SessionId => self.session_id.is_some(),
            Slot::Cwd => self.cwd.is_some(),
            Slot::AddDirs => self.add_dirs.is_some(),
            Slot::WebSearch => self.web_search.is_some(),
            Slot::AllowedTools => self.allowed_tools.is_some(),
            Slot::DisallowedTools => self.disallowed_tools.is_some(),
            Slot::Settings => self.settings.is_some(),
            Slot::SettingSources => self.setting_sources.is_some(),
            Slot::McpConfig => self.mcp_config.is_some(),
            Slot::StrictMcpConfig => self.strict_mcp_config.is_some(),
            Slot::PluginDirs => self.plugin_dirs.is_some(),
            Slot::PluginUrls => self.plugin_urls.is_some(),
            Slot::FallbackModel => self.fallback_model.is_some(),
            Slot::Resume => self.resume.is_some(),
        }
    }

}

impl Slot {
    /// Every slot, in a fixed order, so a new one cannot be added to the flag
    /// table and forgotten by the checks that walk it.
    pub const ALL: [Slot; 20] = [
        Slot::Model,
        Slot::Effort,
        Slot::Permission,
        Slot::Sandbox,
        Slot::Profile,
        Slot::Name,
        Slot::SessionId,
        Slot::Cwd,
        Slot::AddDirs,
        Slot::WebSearch,
        Slot::AllowedTools,
        Slot::DisallowedTools,
        Slot::Settings,
        Slot::SettingSources,
        Slot::McpConfig,
        Slot::StrictMcpConfig,
        Slot::PluginDirs,
        Slot::PluginUrls,
        Slot::FallbackModel,
        Slot::Resume,
    ];
}

impl FlagTable {
    /// Every slot this table populates, so `order` can be checked against it.
    fn populated(&self) -> Vec<Slot> {
        Slot::ALL
            .into_iter()
            .filter(|slot| self.expresses(*slot))
            .collect()
    }

    fn emits(&self) -> Vec<(&'static str, &Emit)> {
        let mut found: Vec<(&'static str, &Emit)> = Vec::new();
        for (key, emit) in [
            ("model", &self.model),
            ("effort", &self.effort),
            ("sandbox", &self.sandbox),
            ("profile", &self.profile),
            ("name", &self.name),
            ("session-id", &self.session_id),
            ("cwd", &self.cwd),
            ("add-dirs", &self.add_dirs),
            ("web-search", &self.web_search),
            ("allowed-tools", &self.allowed_tools),
            ("disallowed-tools", &self.disallowed_tools),
            ("settings", &self.settings),
            ("setting-sources", &self.setting_sources),
            ("mcp-config", &self.mcp_config),
            ("strict-mcp-config", &self.strict_mcp_config),
            ("plugin-dirs", &self.plugin_dirs),
            ("plugin-urls", &self.plugin_urls),
            ("fallback-model", &self.fallback_model),
        ] {
            if let Some(emit) = emit {
                found.push((key, emit));
            }
        }
        if let Some(permission) = &self.permission {
            found.push(("permission.emit", &permission.emit));
            for (key, mode) in [
                ("permission.ask", &permission.ask),
                ("permission.plan", &permission.plan),
                ("permission.accept-edits", &permission.accept_edits),
                ("permission.bypass", &permission.bypass),
            ] {
                if let Some(emit) = mode.as_ref().and_then(|mode| mode.emit.as_ref()) {
                    found.push((key, emit));
                }
            }
        }
        found
    }
}

/// Which capability-probe protocol a family speaks. The manifest names the
/// protocol; [`crate::capabilities`] implements it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeKind {
    /// Claude Code's `--print` JSON output.
    ClaudePrintJson,
    /// Codex's `app-server` JSON-RPC handshake.
    CodexAppServer,
}

/// One agent family, entirely as data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentManifest {
    /// Which family this describes. Manifests configure the families this build
    /// supervises; they do not conjure new ones, because a family also needs a
    /// hook payload parser and a transcript reader.
    pub family: Agent,
    /// Name shown in the UI.
    pub label: String,
    /// The command name a user would type.
    pub command_name: String,
    /// The file stem a native executable must carry.
    pub exe_stem: String,
    /// A substring that must appear, case-insensitively, in a candidate's
    /// `--version` output for it to be this family.
    pub version_marker: String,
    /// Substrings that disqualify a banner even when `version_marker` matched,
    /// so two families can never claim the same binary.
    #[serde(default)]
    pub excluded_markers: Vec<String>,
    /// The environment variable naming this family's own configuration and
    /// signed-in session directory.
    pub home_env: String,
    /// Path segments under `<npm prefix>/node_modules` ending at the directory
    /// holding the native binary. Segments may carry `{node_platform}`,
    /// `{node_arch}` and `{target_triple}`.
    #[serde(default)]
    pub npm_path: Vec<String>,
    pub probe: ProbeKind,
    /// The slots this family emits, in argument-vector order.
    pub order: Vec<Slot>,
    #[serde(default)]
    pub flags: FlagTable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    #[serde(default)]
    agent: Vec<AgentManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("agent manifest is not valid TOML: {0}")]
    Parse(String),
    #[error("agent manifest could not be read: {0}")]
    Read(String),
    #[error("{family}: {slot} is listed in `order` but has no entry in `flags`")]
    SlotWithoutFlag {
        family: &'static str,
        slot: &'static str,
    },
    #[error("{family}: `flags.{slot}` is set but `order` never emits it")]
    FlagWithoutSlot {
        family: &'static str,
        slot: &'static str,
    },
    #[error("{family}: `order` names {slot} twice")]
    DuplicateSlot {
        family: &'static str,
        slot: &'static str,
    },
    #[error("{family}: `flags.{field}` is a switch and cannot also carry `config_key`")]
    SwitchWithConfigKey {
        family: &'static str,
        field: &'static str,
    },
    #[error("{family}: `flags.{field}.flag` is empty")]
    EmptyFlag {
        family: &'static str,
        field: &'static str,
    },
    #[error("{family}: `npm_path` segment {segment:?} uses an unknown placeholder")]
    UnknownPlaceholder {
        family: &'static str,
        segment: String,
    },
    #[error("{family}: `flags.resume.{mode}` has no token containing {{id}}")]
    ResumeWithoutId {
        family: &'static str,
        mode: &'static str,
    },
    #[error("agent manifest declares {0} twice")]
    DuplicateFamily(&'static str),
    #[error("{0}: `label`, `command_name`, `exe_stem`, `version_marker` and `home_env` must all be set")]
    MissingIdentity(&'static str),
}

impl AgentManifest {
    /// A stable name for error messages, independent of the operator's `label`.
    fn family_name(&self) -> &'static str {
        match self.family {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// True when `banner` positively identifies this family.
    pub fn identifies(&self, banner: &str) -> bool {
        let banner = banner.to_ascii_lowercase();
        banner.contains(&self.version_marker.to_ascii_lowercase())
            && !self
                .excluded_markers
                .iter()
                .any(|excluded| banner.contains(&excluded.to_ascii_lowercase()))
    }

    /// The npm package directory holding this family's native binary, with the
    /// platform placeholders resolved for the running target. `None` when a
    /// placeholder has no value here — an unsupported platform, where the npm
    /// route must not be attempted rather than guessed at.
    pub fn npm_directory(&self, prefix: &Path) -> Option<PathBuf> {
        let mut path = prefix.join("node_modules");
        for segment in &self.npm_path {
            path.push(substitute_platform(segment)?);
        }
        Some(path)
    }

    /// Whether this family expresses `slot`.
    pub fn expresses(&self, slot: Slot) -> bool {
        self.flags.expresses(slot)
    }

    /// Check the manifest against itself. Called for the built-ins by their own
    /// test and for every operator-supplied manifest at load.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let family = self.family_name();
        if self.label.trim().is_empty()
            || self.command_name.trim().is_empty()
            || self.exe_stem.trim().is_empty()
            || self.version_marker.trim().is_empty()
            || self.home_env.trim().is_empty()
        {
            return Err(ManifestError::MissingIdentity(family));
        }
        let mut seen: BTreeSet<Slot> = BTreeSet::new();
        for slot in &self.order {
            if !seen.insert(*slot) {
                return Err(ManifestError::DuplicateSlot {
                    family,
                    slot: slot.key(),
                });
            }
            if !self.flags.expresses(*slot) {
                return Err(ManifestError::SlotWithoutFlag {
                    family,
                    slot: slot.key(),
                });
            }
        }
        for slot in self.flags.populated() {
            if !seen.contains(&slot) {
                return Err(ManifestError::FlagWithoutSlot {
                    family,
                    slot: slot.key(),
                });
            }
        }
        for (field, emit) in self.flags.emits() {
            if emit.flag.trim().is_empty() {
                return Err(ManifestError::EmptyFlag { family, field });
            }
            if emit.switch && emit.config_key.is_some() {
                return Err(ManifestError::SwitchWithConfigKey { family, field });
            }
        }
        for segment in &self.npm_path {
            if placeholders(segment).any(|name| !KNOWN_PLACEHOLDERS.contains(&name)) {
                return Err(ManifestError::UnknownPlaceholder {
                    family,
                    segment: segment.clone(),
                });
            }
        }
        if let Some(resume) = &self.flags.resume {
            for (mode, tokens) in [("session", &resume.session), ("fork", &resume.fork)] {
                if let Some(tokens) = tokens {
                    if !tokens.iter().any(|token| token.contains("{id}")) {
                        return Err(ManifestError::ResumeWithoutId { family, mode });
                    }
                }
            }
        }
        Ok(())
    }
}

const KNOWN_PLACEHOLDERS: [&str; 3] = ["node_platform", "node_arch", "target_triple"];

/// Iterate the `{placeholder}` names inside one segment.
fn placeholders(segment: &str) -> impl Iterator<Item = &str> {
    segment.split('{').skip(1).filter_map(|rest| {
        let end = rest.find('}')?;
        Some(&rest[..end])
    })
}

fn substitute_platform(segment: &str) -> Option<String> {
    let mut out = String::with_capacity(segment.len());
    let mut rest = segment;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}')?;
        out.push_str(platform_value(&after[..close])?);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn platform_value(name: &str) -> Option<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match name {
        "node_platform" => Some(match os {
            "windows" => "win32",
            "macos" => "darwin",
            "linux" => "linux",
            _ => return None,
        }),
        "node_arch" => Some(match arch {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => return None,
        }),
        // The triple the vendored binary is filed under. Linux builds are
        // vendored against musl, which is not the host triple.
        "target_triple" => Some(match (os, arch) {
            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
            ("windows", "aarch64") => "aarch64-pc-windows-msvc",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("linux", "x86_64") => "x86_64-unknown-linux-musl",
            ("linux", "aarch64") => "aarch64-unknown-linux-musl",
            _ => return None,
        }),
        _ => None,
    }
}

/// Parse a manifest file's text, validating every family in it.
pub fn parse(text: &str) -> Result<Vec<AgentManifest>, ManifestError> {
    let file: ManifestFile =
        toml_edit::de::from_str(text).map_err(|error| ManifestError::Parse(error.to_string()))?;
    let mut seen: BTreeSet<Agent> = BTreeSet::new();
    for manifest in &file.agent {
        manifest.validate()?;
        if !seen.insert(manifest.family) {
            return Err(ManifestError::DuplicateFamily(manifest.family_name()));
        }
    }
    Ok(file.agent)
}

/// The manifests this binary ships with, parsed once.
///
/// A built-in that does not parse is a build defect, not a runtime condition —
/// `builtin_manifests_are_valid` fails first, so the panic here is unreachable
/// in any binary whose tests ran.
pub fn builtin() -> &'static [AgentManifest] {
    static PARSED: OnceLock<Vec<AgentManifest>> = OnceLock::new();
    PARSED.get_or_init(|| parse(BUILTIN).expect("built-in agent manifests must parse"))
}

/// The active manifest for `agent`.
///
/// Operator overrides are layered over the built-ins at process start by
/// [`install_overrides`]; without one, this is the embedded data.
pub fn for_agent(agent: Agent) -> &'static AgentManifest {
    if let Some(installed) = OVERRIDES.get() {
        if let Some(found) = installed.iter().find(|manifest| manifest.family == agent) {
            return found;
        }
    }
    builtin()
        .iter()
        .find(|manifest| manifest.family == agent)
        .expect("every supervised family has a built-in manifest")
}

static OVERRIDES: OnceLock<Vec<AgentManifest>> = OnceLock::new();

/// Read an operator's manifest file, if they have one.
///
/// A missing file is `Ok(None)`: overriding the built-ins is opt-in. A file that
/// exists and is wrong is an error — silently falling back to the built-ins
/// would launch with a mapping the operator did not choose, which is the same
/// quiet substitution `.terminalai/templates.toml` refuses.
pub fn load_overrides(path: &Path) -> Result<Option<Vec<AgentManifest>>, ManifestError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ManifestError::Read(error.to_string())),
    }
}

/// The operator's manifest path under their data directory.
pub fn override_path() -> Option<PathBuf> {
    dirs::data_dir().map(|base| base.join("TerminalAI").join(OVERRIDE_FILE))
}

/// Install operator overrides for the life of the process. Idempotent; the
/// first caller wins, so a later call cannot re-point a running fleet's flag
/// mapping at a different file.
pub fn install_overrides(manifests: Vec<AgentManifest>) -> bool {
    OVERRIDES.set(manifests).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_manifests_are_valid() {
        let manifests = parse(BUILTIN).expect("built-in manifests parse");
        assert_eq!(manifests.len(), 2, "both supervised families are described");
        for agent in [Agent::Claude, Agent::Codex] {
            let found = manifests
                .iter()
                .find(|manifest| manifest.family == agent)
                .unwrap_or_else(|| panic!("{agent:?} has a manifest"));
            found.validate().expect("built-in manifest validates");
        }
    }

    #[test]
    fn identity_matches_the_verified_banners() {
        let claude = for_agent(Agent::Claude);
        let codex = for_agent(Agent::Codex);
        assert!(claude.identifies("2.1.170 (Claude Code)"));
        assert!(!codex.identifies("2.1.170 (Claude Code)"));
        assert!(codex.identifies("codex-cli 0.146.0"));
        assert!(!claude.identifies("codex-cli 0.146.0"));
        assert!(!claude.identifies(""));
        assert!(!codex.identifies("Node.js v22.11.0"));
    }

    #[test]
    fn a_slot_with_no_flag_entry_is_refused_naming_the_slot() {
        let error = parse(
            r#"
            [[agent]]
            family = "claude"
            label = "Claude Code"
            command-name = "claude"
            exe-stem = "claude"
            version-marker = "claude code"
            home-env = "CLAUDE_CONFIG_DIR"
            probe = "claude-print-json"
            order = ["model", "profile"]
            [agent.flags]
            model = { flag = "--model" }
            "#,
        )
        .unwrap_err();
        assert_eq!(
            error,
            ManifestError::SlotWithoutFlag {
                family: "claude",
                slot: "profile",
            }
        );
    }

    #[test]
    fn a_flag_no_order_emits_is_refused_naming_the_flag() {
        let error = parse(
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
            profile = { flag = "--profile" }
            "#,
        )
        .unwrap_err();
        assert_eq!(
            error,
            ManifestError::FlagWithoutSlot {
                family: "codex",
                slot: "profile",
            }
        );
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        let error = parse(
            r#"
            [[agent]]
            family = "claude"
            label = "Claude Code"
            command-name = "claude"
            exe-stem = "claude"
            version-marker = "claude code"
            home-env = "CLAUDE_CONFIG_DIR"
            probe = "claude-print-json"
            order = []
            dangerously_skip_permissions = true
            "#,
        )
        .unwrap_err();
        match error {
            ManifestError::Parse(message) => assert!(
                message.contains("dangerously_skip_permissions"),
                "the refusal must name the field: {message}"
            ),
            other => panic!("expected a parse refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_resume_mode_that_never_substitutes_the_id_is_refused() {
        let error = parse(
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
            session = ["--resume"]
            "#,
        )
        .unwrap_err();
        assert_eq!(
            error,
            ManifestError::ResumeWithoutId {
                family: "claude",
                mode: "session",
            }
        );
    }

    #[test]
    fn an_unknown_platform_placeholder_is_refused() {
        let error = parse(
            r#"
            [[agent]]
            family = "codex"
            label = "Codex"
            command-name = "codex"
            exe-stem = "codex"
            version-marker = "codex"
            home-env = "CODEX_HOME"
            probe = "codex-app-server"
            npm-path = ["@openai", "codex-{libc}"]
            order = []
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ManifestError::UnknownPlaceholder { family: "codex", .. }
        ));
    }

    #[test]
    fn the_npm_path_resolves_for_this_platform() {
        let codex = for_agent(Agent::Codex);
        let directory = codex
            .npm_directory(Path::new("prefix"))
            .expect("this platform is one the vendored table covers");
        let rendered = directory.to_string_lossy().replace('\\', "/");
        assert!(
            rendered.starts_with("prefix/node_modules/@openai/codex/"),
            "unexpected npm directory: {rendered}"
        );
        assert!(
            !rendered.contains('{'),
            "every placeholder must be substituted: {rendered}"
        );
    }

    #[test]
    fn a_missing_override_file_is_not_an_error() {
        let missing = std::env::temp_dir().join("terminalai-no-such-agents.toml");
        assert_eq!(load_overrides(&missing), Ok(None));
    }

    #[test]
    fn every_slot_names_an_option_and_a_key() {
        // The two spellings are used in refusals; an empty one would produce a
        // message naming nothing.
        for slot in [
            Slot::Model,
            Slot::Effort,
            Slot::Permission,
            Slot::Sandbox,
            Slot::Profile,
            Slot::Name,
            Slot::SessionId,
            Slot::Cwd,
            Slot::AddDirs,
            Slot::WebSearch,
            Slot::Resume,
        ] {
            assert!(!slot.option_name().is_empty());
            assert!(!slot.key().is_empty());
        }
    }
}
