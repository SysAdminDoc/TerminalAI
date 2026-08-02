//! Locating the agent executables.
//!
//! On Windows both CLIs are installed as npm packages, but neither `claude` nor
//! `codex` on `PATH` is something `CreateProcess` can execute: they are `.cmd`
//! shims. `claude.cmd` wraps a real `claude.exe`; `codex.cmd` wraps
//! `node.exe codex.js`, which in turn re-spawns a per-platform `codex.exe`.
//!
//! We resolve straight to the native binary. That skips a `cmd.exe`, skips a
//! Node process for Codex, and — more importantly — avoids `cmd.exe`'s quoting
//! rules mangling prompts that contain `&`, `^`, `|` or `%`.

use std::path::{Path, PathBuf};

/// Which coding agent a session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// Name shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    /// The command name a user would type.
    pub fn command_name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// Models offered in the launcher dropdown. Free-text entry stays available
    /// in the GUI, because both CLIs accept aliases we do not enumerate here.
    pub fn suggested_models(self) -> &'static [&'static str] {
        match self {
            Agent::Claude => &["opus", "sonnet", "haiku"],
            Agent::Codex => &["gpt-5.1-codex", "gpt-5.1-codex-mini", "gpt-5.1"],
        }
    }
}

/// A resolved, directly spawnable agent executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBinary {
    pub agent: Agent,
    pub path: PathBuf,
    /// How we found it — surfaced in the UI so a wrong pick is diagnosable.
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Explicitly configured by the user.
    Configured,
    /// Found under the global npm prefix.
    NpmPrefix,
    /// Found on `PATH` as a real executable.
    Path,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("could not find a native executable for {0}; set its path in Settings")]
    NotFound(&'static str),
    #[error("configured path for {agent} does not exist: {path}")]
    ConfiguredMissing { agent: &'static str, path: PathBuf },
}

/// Resolve `agent` to a native executable.
///
/// Order: explicit override, global npm prefix, then `PATH`. The `PATH` step
/// looks for `claude.exe` / `codex.exe` specifically — a `.cmd` hit is rejected
/// because we cannot spawn it without `cmd.exe`.
pub fn resolve(agent: Agent, configured: Option<&Path>) -> Result<AgentBinary, ResolveError> {
    if let Some(p) = configured {
        return if p.is_file() {
            Ok(AgentBinary { agent, path: p.to_path_buf(), origin: Origin::Configured })
        } else {
            Err(ResolveError::ConfiguredMissing {
                agent: agent.command_name(),
                path: p.to_path_buf(),
            })
        };
    }

    if let Some(path) = npm_prefix().and_then(|prefix| in_npm_prefix(agent, &prefix)) {
        return Ok(AgentBinary { agent, path, origin: Origin::NpmPrefix });
    }

    let exe = format!("{}{}", agent.command_name(), std::env::consts::EXE_SUFFIX);
    if let Ok(path) = which::which(&exe) {
        if is_native_executable(&path) {
            return Ok(AgentBinary { agent, path, origin: Origin::Path });
        }
    }

    Err(ResolveError::NotFound(agent.command_name()))
}

/// The global npm prefix, without shelling out to `npm prefix -g` (which costs
/// ~400ms of Node startup every launch).
fn npm_prefix() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NPM_CONFIG_PREFIX") {
        return Some(PathBuf::from(p));
    }
    if cfg!(windows) {
        dirs::data_dir().map(|d| d.join("npm"))
    } else {
        // `/usr/local` and `~/.npm-global` are the usual two; try the local one
        // first since a user-level install shadows the system one on PATH.
        let home = dirs::home_dir()?;
        let local = home.join(".npm-global");
        if local.is_dir() {
            Some(local)
        } else {
            Some(PathBuf::from("/usr/local"))
        }
    }
}

fn in_npm_prefix(agent: Agent, prefix: &Path) -> Option<PathBuf> {
    let modules = prefix.join("node_modules");
    let candidate = match agent {
        Agent::Claude => modules
            .join("@anthropic-ai")
            .join("claude-code")
            .join("bin")
            .join(format!("claude{}", std::env::consts::EXE_SUFFIX)),
        Agent::Codex => {
            // The launcher package depends on a per-platform package that ships
            // the real binary under `vendor/<target-triple>/bin/`.
            let vendor = modules
                .join("@openai")
                .join("codex")
                .join("node_modules")
                .join("@openai")
                .join(codex_platform_package()?)
                .join("vendor")
                .join(codex_target_triple()?)
                .join("bin");
            vendor.join(format!("codex{}", std::env::consts::EXE_SUFFIX))
        }
    };
    candidate.is_file().then_some(candidate)
}

fn codex_platform_package() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "codex-win32-x64",
        ("windows", "aarch64") => "codex-win32-arm64",
        ("macos", "x86_64") => "codex-darwin-x64",
        ("macos", "aarch64") => "codex-darwin-arm64",
        ("linux", "x86_64") => "codex-linux-x64",
        ("linux", "aarch64") => "codex-linux-arm64",
        _ => return None,
    })
}

fn codex_target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        _ => return None,
    })
}

/// Reject the npm `.cmd` / `.ps1` shims — `CreateProcess` cannot run them.
fn is_native_executable(path: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("exe") | Some("com")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_shims_are_not_spawnable() {
        // The whole reason this module exists.
        assert!(!is_native_executable(Path::new(r"C:\npm\claude.cmd")));
        assert!(!is_native_executable(Path::new(r"C:\npm\claude.ps1")));
        assert!(is_native_executable(Path::new(r"C:\npm\claude.exe")));
    }

    #[test]
    fn configured_path_must_exist() {
        let err = resolve(Agent::Claude, Some(Path::new("/definitely/not/here"))).unwrap_err();
        assert!(matches!(err, ResolveError::ConfiguredMissing { .. }));
    }

    #[test]
    fn platform_tables_agree() {
        // Both tables must resolve, or neither — a half-populated match arm
        // would silently fall through to the PATH branch.
        assert_eq!(codex_platform_package().is_some(), codex_target_triple().is_some());
    }
}
