//! Versioned agent/launcher compatibility fixtures.
//!
//! The launch goldens are intentionally data rather than a second collection of
//! Rust builders. The core golden tests and `terminalai-probe verify-goldens`
//! both consume these cases, so a preview and a real launch cannot quietly drift
//! into different compatibility claims.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agent::Agent;
use crate::launch::LaunchSpec;

pub const MATRIX_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct CompatibilityFixture {
    pub schema_version: u32,
    pub agent: Agent,
    /// Human-readable fixture label, for example `Claude Code 2.1.170`.
    pub version: String,
    /// A substring of the installed CLI's `--version` banner. Keeping this
    /// separate from `version` lets reports stay readable without weakening the
    /// exact-version selection made by the probe.
    pub agent_version: String,
    #[serde(default)]
    pub cases: Vec<CompatibilityCase>,
    #[serde(default)]
    pub vendor: VendorCompatibility,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompatibilityCase {
    pub id: String,
    /// The launcher field or feature this case protects. It is deliberately
    /// human-readable so a failed matrix report tells an operator what changed.
    pub capability: String,
    pub status: CompatibilityStatus,
    pub spec: LaunchSpec,
    #[serde(default)]
    pub expected_args: Vec<String>,
    #[serde(default)]
    pub error_contains: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityStatus {
    Accepted,
    Unsupported,
    ModeRestricted,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VendorCompatibility {
    /// Flags the installed CLI must list and that TerminalAI emits in an
    /// accepted case. This supplements the derived argv check with explicit
    /// capability coverage for a flag whose value shape is unusual.
    #[serde(default)]
    pub accepted_flags: Vec<String>,
    /// Flags the pinned CLI does not advertise. TerminalAI must refuse their
    /// corresponding launcher choice rather than silently dropping it.
    #[serde(default)]
    pub unsupported_flags: Vec<String>,
    /// Flags the CLI lists but only applies in another invocation mode.
    #[serde(default)]
    pub mode_restricted: Vec<ModeRestriction>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModeRestriction {
    pub flag: String,
    pub requires: String,
}

impl CompatibilityFixture {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != MATRIX_SCHEMA_VERSION {
            return Err(format!(
                "unsupported compatibility fixture schema {}; expected {}",
                self.schema_version, MATRIX_SCHEMA_VERSION
            ));
        }
        if self.version.trim().is_empty() || self.agent_version.trim().is_empty() {
            return Err("compatibility fixture must pin a human and CLI version".to_owned());
        }
        if self.cases.is_empty() {
            return Err("compatibility fixture must contain at least one case".to_owned());
        }
        let mut accepted = 0;
        for case in &self.cases {
            if case.id.trim().is_empty() || case.capability.trim().is_empty() {
                return Err("compatibility cases need an id and capability".to_owned());
            }
            if case.spec.agent != self.agent {
                return Err(format!(
                    "case {} names {:?}, but fixture is {:?}",
                    case.id, case.spec.agent, self.agent
                ));
            }
            match case.status {
                CompatibilityStatus::Accepted => {
                    accepted += 1;
                    if case.expected_args.is_empty() {
                        return Err(format!(
                            "accepted compatibility case {} has no expected argv",
                            case.id
                        ));
                    }
                    if case.error_contains.is_some() {
                        return Err(format!(
                            "accepted compatibility case {} carries an error expectation",
                            case.id
                        ));
                    }
                }
                CompatibilityStatus::Unsupported | CompatibilityStatus::ModeRestricted => {
                    if case.error_contains.is_none() {
                        return Err(format!(
                            "rejected compatibility case {} needs error_contains",
                            case.id
                        ));
                    }
                }
            }
        }
        if accepted == 0 {
            return Err("compatibility fixture must contain an accepted case".to_owned());
        }
        Ok(())
    }

    /// Expand repository-relative placeholders in one case without allowing a
    /// fixture to influence any other path.
    pub fn expand_spec(&self, case: &CompatibilityCase, root: &Path) -> LaunchSpec {
        let mut spec = case.spec.clone();
        spec.cwd = expand_path(&spec.cwd, root);
        spec.add_dirs = spec
            .add_dirs
            .iter()
            .map(|path| expand_path(path, root))
            .collect();
        spec.plugin_dirs = spec
            .plugin_dirs
            .iter()
            .map(|path| expand_path(path, root))
            .collect();
        spec.agent_home = spec.agent_home.as_ref().map(|path| expand_path(path, root));
        spec
    }

    pub fn expand_args(&self, case: &CompatibilityCase, root: &Path) -> Vec<String> {
        case.expected_args
            .iter()
            .map(|arg| expand_token(arg, root))
            .collect()
    }
}

fn expand_path(path: &Path, root: &Path) -> PathBuf {
    PathBuf::from(expand_token(&path.to_string_lossy(), root))
}

fn expand_token(token: &str, root: &Path) -> String {
    if !token.contains("__CARGO_MANIFEST_DIR__") {
        return token.to_owned();
    }
    let expanded = token.replace("__CARGO_MANIFEST_DIR__", &root.to_string_lossy());
    expanded.replace('/', std::path::MAIN_SEPARATOR_STR)
}
