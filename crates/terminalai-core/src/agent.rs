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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// How long a `--version` identification probe may take before the candidate is
/// treated as unidentifiable. Both CLIs answer in well under a second.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Which coding agent a session runs.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
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

    /// The file stem a native executable for this agent must carry. A configured
    /// path whose stem differs is refused before anything is executed.
    pub fn expected_exe_stem(self) -> &'static str {
        self.command_name()
    }

    /// A substring that must appear, case-insensitively, in the candidate's
    /// `--version` output. Verified 2026-08-03 against Claude Code 2.1.170
    /// (`2.1.170 (Claude Code)`) and codex-cli 0.146.0 (`codex-cli 0.146.0`).
    pub fn version_marker(self) -> &'static str {
        match self {
            Agent::Claude => "claude code",
            Agent::Codex => "codex",
        }
    }

    /// True when `banner` positively identifies this agent.
    pub fn identifies(self, banner: &str) -> bool {
        let banner = banner.to_ascii_lowercase();
        if !banner.contains(self.version_marker()) {
            return false;
        }
        // "codex" appears in Claude's own output only as a model alias, never in
        // its version banner — but keep the sibling exclusion explicit so a future
        // banner change cannot make both agents match the same string.
        match self {
            Agent::Claude => true,
            Agent::Codex => !banner.contains("claude code"),
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
    #[error(
        "configured path for {agent} is named {found}, but {agent} runs as {expected}{exe_suffix}: {path}"
    )]
    ConfiguredWrongName {
        agent: &'static str,
        expected: &'static str,
        exe_suffix: &'static str,
        found: String,
        path: PathBuf,
    },
    #[error(
        "configured path for {agent} is a different CLI: {path} answered --version with {banner:?}"
    )]
    ConfiguredWrongBinary {
        agent: &'static str,
        path: PathBuf,
        banner: String,
    },
    #[error("configured path for {agent} could not be identified: {path} ({cause})")]
    ConfiguredUnidentifiable {
        agent: &'static str,
        path: PathBuf,
        cause: String,
    },
}

/// Resolve `agent` to a native executable.
///
/// Order: explicit override, global npm prefix, then `PATH`. The `PATH` step
/// looks for `claude.exe` / `codex.exe` specifically — a `.cmd` hit is rejected
/// because we cannot spawn it without `cmd.exe`.
///
/// A configured path is *identified*, not trusted: the npm-prefix and `PATH`
/// routes derive the file name from the agent, so their stem matches by
/// construction, but a configured path is whatever the operator typed. It is
/// accepted only after its file stem matches and its `--version` banner names
/// the agent that was asked for. Without that, the downstream
/// `spec.agent != binary.agent` guard compares two values that are equal by
/// construction and can never fire.
pub fn resolve(agent: Agent, configured: Option<&Path>) -> Result<AgentBinary, ResolveError> {
    resolve_with_probe(agent, configured, cached_version_banner)
}

fn resolve_with_probe(
    agent: Agent,
    configured: Option<&Path>,
    probe: impl Fn(&Path) -> Result<String, String>,
) -> Result<AgentBinary, ResolveError> {
    if let Some(p) = configured {
        return identify_configured(agent, p, probe);
    }

    if let Some(path) = npm_prefix().and_then(|prefix| in_npm_prefix(agent, &prefix)) {
        return Ok(AgentBinary {
            agent,
            path,
            origin: Origin::NpmPrefix,
        });
    }

    let exe = format!("{}{}", agent.command_name(), std::env::consts::EXE_SUFFIX);
    if let Ok(path) = which::which(&exe) {
        if is_native_executable(&path) {
            return Ok(AgentBinary {
                agent,
                path,
                origin: Origin::Path,
            });
        }
    }

    Err(ResolveError::NotFound(agent.command_name()))
}

/// Positively identify an operator-configured executable before it is ever spawned.
fn identify_configured(
    agent: Agent,
    path: &Path,
    probe: impl Fn(&Path) -> Result<String, String>,
) -> Result<AgentBinary, ResolveError> {
    if !path.is_file() {
        return Err(ResolveError::ConfiguredMissing {
            agent: agent.command_name(),
            path: path.to_path_buf(),
        });
    }
    if !is_native_executable(path) {
        return Err(ResolveError::ConfiguredWrongName {
            agent: agent.command_name(),
            expected: agent.expected_exe_stem(),
            exe_suffix: std::env::consts::EXE_SUFFIX,
            found: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_path_buf(),
        });
    }
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !stem.eq_ignore_ascii_case(agent.expected_exe_stem()) {
        return Err(ResolveError::ConfiguredWrongName {
            agent: agent.command_name(),
            expected: agent.expected_exe_stem(),
            exe_suffix: std::env::consts::EXE_SUFFIX,
            found: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_path_buf(),
        });
    }
    let banner = probe(path).map_err(|cause| ResolveError::ConfiguredUnidentifiable {
        agent: agent.command_name(),
        path: path.to_path_buf(),
        cause,
    })?;
    if !agent.identifies(&banner) {
        return Err(ResolveError::ConfiguredWrongBinary {
            agent: agent.command_name(),
            path: path.to_path_buf(),
            banner: banner.trim().to_owned(),
        });
    }
    Ok(AgentBinary {
        agent,
        path: path.to_path_buf(),
        origin: Origin::Configured,
    })
}

/// Cache key: the path plus its size and mtime, so replacing the binary in place
/// re-probes instead of trusting a banner from the executable that used to live
/// there.
type ProbeKey = (PathBuf, u64, Option<SystemTime>);

fn probe_cache() -> &'static Mutex<HashMap<ProbeKey, Result<String, String>>> {
    static CACHE: OnceLock<Mutex<HashMap<ProbeKey, Result<String, String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn probe_key(path: &Path) -> ProbeKey {
    let meta = path.metadata().ok();
    (
        path.to_path_buf(),
        meta.as_ref().map(|m| m.len()).unwrap_or(0),
        meta.as_ref().and_then(|m| m.modified().ok()),
    )
}

fn cached_version_banner(path: &Path) -> Result<String, String> {
    let key = probe_key(path);
    if let Ok(cache) = probe_cache().lock() {
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
    }
    let result = version_banner(path);
    if let Ok(mut cache) = probe_cache().lock() {
        // A launcher only ever configures a handful of paths; the bound exists so
        // a pathological caller cannot grow this without limit.
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, result.clone());
    }
    result
}

/// Run `<path> --version` with a deadline and return its combined output.
///
/// Polled rather than blocked on purpose: a candidate that is not the CLI we
/// expect may never exit, and a blocking `output()` would hang the launcher.
/// Both real banners are a single short line, so the pipe cannot fill before the
/// process exits; a candidate that floods stdout instead trips the deadline and
/// is killed, which is the correct answer for an unidentifiable binary.
pub(crate) fn version_banner(path: &Path) -> Result<String, String> {
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not run --version: {error}"))?;
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                return Err(format!("could not wait for --version: {error}"));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "--version did not answer within {}s",
                VERSION_PROBE_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("could not read --version: {error}"))?;
    let mut banner = String::from_utf8_lossy(&output.stdout).into_owned();
    if banner.trim().is_empty() {
        banner = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    if banner.trim().is_empty() {
        return Err("--version produced no output".to_owned());
    }
    Ok(banner)
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
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
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

    /// A real file whose name we control, so identification can be exercised
    /// without depending on anything installed on the machine.
    fn scratch_binary(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("terminalai-agent-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(name);
        std::fs::write(&path, b"not a real executable").expect("scratch file");
        path
    }

    #[test]
    fn configured_claude_path_pointing_at_codex_is_refused() {
        let codex = scratch_binary(&format!("codex{}", std::env::consts::EXE_SUFFIX));
        let err = resolve_with_probe(Agent::Claude, Some(&codex), |_| {
            panic!("must not execute a candidate whose name already disqualifies it")
        })
        .unwrap_err();
        match err {
            ResolveError::ConfiguredWrongName {
                agent, expected, ..
            } => {
                assert_eq!(agent, "claude");
                assert_eq!(expected, "claude");
            }
            other => panic!("expected a name refusal, got {other:?}"),
        }
    }

    #[test]
    fn configured_path_pointing_at_an_unrelated_executable_is_refused() {
        let other = scratch_binary(&format!("notepad{}", std::env::consts::EXE_SUFFIX));
        let err = resolve_with_probe(Agent::Claude, Some(&other), |_| {
            panic!("must not execute an unrelated binary")
        })
        .unwrap_err();
        assert!(matches!(err, ResolveError::ConfiguredWrongName { .. }));
    }

    #[test]
    fn a_correctly_named_impostor_is_refused_by_its_banner() {
        // The case the file-name check alone cannot catch: codex.exe renamed to
        // claude.exe. Only running it settles which CLI it is.
        let impostor = scratch_binary(&format!("claude{}", std::env::consts::EXE_SUFFIX));
        let err = resolve_with_probe(Agent::Claude, Some(&impostor), |_| {
            Ok("codex-cli 0.146.0".to_owned())
        })
        .unwrap_err();
        match err {
            ResolveError::ConfiguredWrongBinary { agent, banner, .. } => {
                assert_eq!(agent, "claude");
                assert_eq!(banner, "codex-cli 0.146.0");
            }
            other => panic!("expected a banner refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_unrunnable_candidate_is_refused_rather_than_assumed_good() {
        let candidate = scratch_binary(&format!("codex{}", std::env::consts::EXE_SUFFIX));
        let err = resolve_with_probe(Agent::Codex, Some(&candidate), |_| {
            Err("--version produced no output".to_owned())
        })
        .unwrap_err();
        assert!(matches!(
            err,
            ResolveError::ConfiguredUnidentifiable { .. }
        ));
    }

    #[test]
    fn a_matching_banner_is_accepted() {
        for (agent, name, banner) in [
            (Agent::Claude, "claude", "2.1.170 (Claude Code)"),
            (Agent::Codex, "codex", "codex-cli 0.146.0"),
        ] {
            let path = scratch_binary(&format!("{name}{}", std::env::consts::EXE_SUFFIX));
            let resolved =
                resolve_with_probe(agent, Some(&path), |_| Ok(banner.to_owned())).unwrap();
            assert_eq!(resolved.agent, agent);
            assert_eq!(resolved.origin, Origin::Configured);
            assert_eq!(resolved.path, path);
        }
    }

    /// Exercises the real `--version` probe against whatever is installed, so the
    /// identification cannot pass only against synthetic banners. Skips silently
    /// when an agent is absent — CI machines have neither CLI.
    #[test]
    fn installed_agents_identify_themselves_through_the_real_probe() {
        for agent in [Agent::Claude, Agent::Codex] {
            let Ok(found) = resolve(agent, None) else {
                continue;
            };
            let reconfigured = resolve(agent, Some(&found.path))
                .unwrap_or_else(|error| panic!("{} rejected its own binary: {error}", agent.label()));
            assert_eq!(reconfigured.path, found.path);
            assert_eq!(reconfigured.origin, Origin::Configured);

            let sibling = match agent {
                Agent::Claude => Agent::Codex,
                Agent::Codex => Agent::Claude,
            };
            let err = resolve(sibling, Some(&found.path))
                .expect_err("one agent's binary must never resolve as the other");
            assert!(matches!(
                err,
                ResolveError::ConfiguredWrongName { .. } | ResolveError::ConfiguredWrongBinary { .. }
            ));
        }
    }

    #[test]
    fn banners_identify_exactly_one_agent() {
        assert!(Agent::Claude.identifies("2.1.170 (Claude Code)"));
        assert!(!Agent::Codex.identifies("2.1.170 (Claude Code)"));
        assert!(Agent::Codex.identifies("codex-cli 0.146.0"));
        assert!(!Agent::Claude.identifies("codex-cli 0.146.0"));
        assert!(!Agent::Claude.identifies(""));
        assert!(!Agent::Codex.identifies("Node.js v22.11.0"));
    }

    #[cfg(windows)]
    #[test]
    fn a_shim_configured_by_hand_is_refused_before_it_runs() {
        // `claude.cmd` is on PATH and looks right to an operator, but it cannot be
        // spawned without cmd.exe — the whole reason this module resolves natively.
        let shim = scratch_binary("claude.cmd");
        let err = resolve_with_probe(Agent::Claude, Some(&shim), |_| {
            panic!("must not execute a shim")
        })
        .unwrap_err();
        assert!(matches!(err, ResolveError::ConfiguredWrongName { .. }));
    }

    #[test]
    fn platform_tables_agree() {
        // Both tables must resolve, or neither — a half-populated match arm
        // would silently fall through to the PATH branch.
        assert_eq!(
            codex_platform_package().is_some(),
            codex_target_triple().is_some()
        );
    }
}
