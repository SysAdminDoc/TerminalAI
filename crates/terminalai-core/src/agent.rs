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
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use which::sys::{Sys, SysMetadata};

use crate::manifest::{self, AgentManifest};

/// How long a `--version` identification probe may take before the candidate is
/// treated as unidentifiable. Both CLIs answer in well under a second.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Which coding agent a session runs.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
}

impl Agent {
    /// Every family this tool supervises, so a check that has to cover all of
    /// them cannot be written against a list that quietly falls behind.
    pub const ALL: [Agent; 2] = [Agent::Claude, Agent::Codex];

    /// Everything about this family that is data: names, banners, npm layout
    /// and the flag spellings the launcher emits.
    pub fn manifest(self) -> &'static AgentManifest {
        manifest::for_agent(self)
    }

    /// Name shown in the UI.
    pub fn label(self) -> &'static str {
        &self.manifest().label
    }

    /// The command name a user would type.
    pub fn command_name(self) -> &'static str {
        &self.manifest().command_name
    }

    /// The file stem a native executable for this agent must carry. A configured
    /// path whose stem differs is refused before anything is executed.
    pub fn expected_exe_stem(self) -> &'static str {
        &self.manifest().exe_stem
    }

    /// A substring that must appear, case-insensitively, in the candidate's
    /// `--version` output. Verified 2026-08-03 against Claude Code 2.1.170
    /// (`2.1.170 (Claude Code)`) and codex-cli 0.146.0 (`codex-cli 0.146.0`).
    pub fn version_marker(self) -> &'static str {
        &self.manifest().version_marker
    }

    /// The environment variable naming this agent's own configuration and
    /// signed-in session directory.
    pub fn home_env(self) -> &'static str {
        &self.manifest().home_env
    }

    /// True when `banner` positively identifies this agent.
    pub fn identifies(self, banner: &str) -> bool {
        self.manifest().identifies(banner)
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
    resolve_with(&which::sys::RealSys, agent, configured, probe)
}

/// The environment resolution reads, behind a trait so it can be faked.
///
/// `which`'s own `Sys` covers the filesystem, `PATH` and `PATHEXT`; resolution
/// additionally reads `NPM_CONFIG_PREFIX` and the roaming-data directory, so
/// this extends it rather than replacing it. Without this seam the npm-prefix
/// and `PATH` routes are only reachable through whatever happens to be
/// installed on the machine running the tests.
pub(crate) trait ResolveSys: Sys {
    /// Read an arbitrary environment variable.
    fn env_var(&self, key: &str) -> Option<OsString>;
    /// Roaming application data, where npm keeps its global prefix on Windows.
    fn data_dir(&self) -> Option<PathBuf>;

    /// True when `path` names an existing file. A missing path is `false`, not
    /// an error — every caller here treats the two the same way.
    fn path_is_file(&self, path: &Path) -> bool {
        self.metadata(path).map(|m| m.is_file()).unwrap_or(false)
    }

    /// The executable suffix for the target this `Sys` describes.
    fn exe_suffix(&self) -> &'static str {
        if self.is_windows() {
            ".exe"
        } else {
            ""
        }
    }
}

impl ResolveSys for which::sys::RealSys {
    fn env_var(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }

    fn data_dir(&self) -> Option<PathBuf> {
        dirs::data_dir()
    }
}

fn resolve_with<S: ResolveSys>(
    sys: &S,
    agent: Agent,
    configured: Option<&Path>,
    probe: impl Fn(&Path) -> Result<String, String>,
) -> Result<AgentBinary, ResolveError> {
    if let Some(p) = configured {
        return identify_configured(sys, agent, p, probe);
    }

    if let Some(path) = npm_prefix(sys).and_then(|prefix| in_npm_prefix(sys, agent, &prefix)) {
        return Ok(AgentBinary {
            agent,
            path,
            origin: Origin::NpmPrefix,
        });
    }

    if let Some(path) = on_path(sys, agent) {
        return Ok(AgentBinary {
            agent,
            path,
            origin: Origin::Path,
        });
    }

    Err(ResolveError::NotFound(agent.command_name()))
}

/// Search `PATH` for the agent's native executable.
///
/// The query carries an explicit `.exe`, which is what makes an unpopulated
/// Windows `PATHEXT` harmless: `which` only synthesises extensions for a query
/// that lacks one, and falls back to the literal name when the list is empty.
/// Non-fatal search errors are collected so a miss can be explained rather than
/// being indistinguishable from "not installed".
fn on_path<S: ResolveSys>(sys: &S, agent: Agent) -> Option<PathBuf> {
    let exe = format!("{}{}", agent.command_name(), sys.exe_suffix());
    let mut notes: Vec<String> = Vec::new();
    let found = which::WhichConfig::new_with_sys(sys)
        .binary_name(OsString::from(&exe))
        .nonfatal_error_handler(|error: which::NonFatalError| notes.push(error.to_string()))
        .first_result()
        .ok();
    match found {
        // A `.cmd` cannot be reached through this query, but a directory or a
        // non-executable file with the right name can be.
        Some(path) if is_native_executable(sys, &path) => Some(path),
        _ => {
            for note in notes {
                tracing::debug!(agent = agent.command_name(), "PATH search: {note}");
            }
            None
        }
    }
}

/// Positively identify an operator-configured executable before it is ever spawned.
fn identify_configured<S: ResolveSys>(
    sys: &S,
    agent: Agent,
    path: &Path,
    probe: impl Fn(&Path) -> Result<String, String>,
) -> Result<AgentBinary, ResolveError> {
    if !sys.path_is_file(path) {
        return Err(ResolveError::ConfiguredMissing {
            agent: agent.command_name(),
            path: path.to_path_buf(),
        });
    }
    if !is_native_executable(sys, path) {
        return Err(ResolveError::ConfiguredWrongName {
            agent: agent.command_name(),
            expected: agent.expected_exe_stem(),
            exe_suffix: sys.exe_suffix(),
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
            exe_suffix: sys.exe_suffix(),
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
pub fn version_banner(path: &Path) -> Result<String, String> {
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
fn npm_prefix<S: ResolveSys>(sys: &S) -> Option<PathBuf> {
    if let Some(p) = sys.env_var("NPM_CONFIG_PREFIX") {
        return Some(PathBuf::from(p));
    }
    if sys.is_windows() {
        sys.data_dir().map(|d| d.join("npm"))
    } else {
        // `/usr/local` and `~/.npm-global` are the usual two; try the local one
        // first since a user-level install shadows the system one on PATH.
        let home = sys.home_dir()?;
        let local = home.join(".npm-global");
        if sys.metadata(&local).map(|m| !m.is_file()).unwrap_or(false) {
            Some(local)
        } else {
            Some(PathBuf::from("/usr/local"))
        }
    }
}

fn in_npm_prefix<S: ResolveSys>(sys: &S, agent: Agent, prefix: &Path) -> Option<PathBuf> {
    let manifest = agent.manifest();
    let directory = manifest.npm_directory(prefix)?;
    let candidate = directory.join(format!("{}{}", manifest.exe_stem, sys.exe_suffix()));
    sys.path_is_file(&candidate).then_some(candidate)
}

/// Reject the npm `.cmd` / `.ps1` shims — `CreateProcess` cannot run them.
fn is_native_executable<S: ResolveSys>(sys: &S, path: &Path) -> bool {
    if !sys.is_windows() {
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

    /// A filesystem and environment that exists only for the duration of a test.
    ///
    /// The npm-prefix and `PATH` routes used to be reachable only through
    /// whatever happened to be installed on the machine running the suite, which
    /// meant a resolution miss looked exactly like an uninstalled agent. `which`
    /// 8's `Sys` trait makes both routes injectable.
    #[derive(Default)]
    struct FakeSys {
        files: Vec<PathBuf>,
        dirs: Vec<PathBuf>,
        env: HashMap<String, OsString>,
        windows: bool,
        home: Option<PathBuf>,
    }

    struct FakeMeta {
        is_file: bool,
    }

    impl SysMetadata for FakeMeta {
        fn is_symlink(&self) -> bool {
            false
        }
        fn is_file(&self) -> bool {
            self.is_file
        }
    }

    struct FakeEntry(PathBuf);

    impl which::sys::SysReadDirEntry for FakeEntry {
        fn file_name(&self) -> OsString {
            self.0.file_name().unwrap_or_default().to_os_string()
        }
        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl FakeSys {
        fn windows() -> Self {
            Self {
                windows: true,
                ..Default::default()
            }
        }

        fn with_file(mut self, path: &str) -> Self {
            self.files.push(PathBuf::from(path));
            self
        }

        fn with_dir(mut self, path: &str) -> Self {
            self.dirs.push(PathBuf::from(path));
            self
        }

        fn with_env(mut self, key: &str, value: &str) -> Self {
            self.env.insert(key.to_owned(), OsString::from(value));
            self
        }

        /// Compare case-insensitively on Windows, matching NTFS.
        fn holds(&self, list: &[PathBuf], path: &Path) -> bool {
            list.iter().any(|known| {
                if self.windows {
                    known.to_string_lossy().eq_ignore_ascii_case(
                        path.to_string_lossy().replace('/', "\\").as_str(),
                    )
                } else {
                    known == path
                }
            })
        }
    }

    impl Sys for FakeSys {
        type ReadDirEntry = FakeEntry;
        type Metadata = FakeMeta;

        fn is_windows(&self) -> bool {
            self.windows
        }
        fn current_dir(&self) -> std::io::Result<PathBuf> {
            Ok(PathBuf::from(if self.windows { r"C:\work" } else { "/work" }))
        }
        fn home_dir(&self) -> Option<PathBuf> {
            self.home.clone()
        }
        fn env_split_paths(&self, paths: &std::ffi::OsStr) -> Vec<PathBuf> {
            let sep = if self.windows { ';' } else { ':' };
            paths
                .to_string_lossy()
                .split(sep)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        }
        fn env_path(&self) -> Option<OsString> {
            self.env.get("PATH").cloned()
        }
        fn env_path_ext(&self) -> Option<OsString> {
            self.env.get("PATHEXT").cloned()
        }
        fn metadata(&self, path: &Path) -> std::io::Result<Self::Metadata> {
            if self.holds(&self.files, path) {
                Ok(FakeMeta { is_file: true })
            } else if self.holds(&self.dirs, path) {
                Ok(FakeMeta { is_file: false })
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    path.display().to_string(),
                ))
            }
        }
        fn symlink_metadata(&self, path: &Path) -> std::io::Result<Self::Metadata> {
            self.metadata(path)
        }
        fn read_dir(
            &self,
            path: &Path,
        ) -> std::io::Result<Box<dyn Iterator<Item = std::io::Result<Self::ReadDirEntry>>>> {
            let children: Vec<_> = self
                .files
                .iter()
                .filter(|f| f.parent() == Some(path))
                .map(|f| Ok(FakeEntry(f.clone())))
                .collect();
            Ok(Box::new(children.into_iter()))
        }
        fn is_valid_executable(&self, path: &Path) -> std::io::Result<bool> {
            Ok(self.holds(&self.files, path))
        }
    }

    impl ResolveSys for FakeSys {
        fn env_var(&self, key: &str) -> Option<OsString> {
            self.env.get(key).cloned()
        }
        fn data_dir(&self) -> Option<PathBuf> {
            self.env.get("APPDATA").map(PathBuf::from)
        }
    }

    fn never_probed(_: &Path) -> Result<String, String> {
        panic!("a route that derives the file name from the agent must not need a probe")
    }

    #[test]
    fn cmd_shims_are_not_spawnable() {
        // The whole reason this module exists.
        let sys = FakeSys::windows();
        assert!(!is_native_executable(&sys, Path::new(r"C:\npm\claude.cmd")));
        assert!(!is_native_executable(&sys, Path::new(r"C:\npm\claude.ps1")));
        assert!(is_native_executable(&sys, Path::new(r"C:\npm\claude.exe")));
    }

    #[test]
    fn path_search_finds_the_native_executable() {
        let sys = FakeSys::windows()
            .with_env("PATH", r"C:\tools;C:\npm")
            .with_env("PATHEXT", ".COM;.EXE;.CMD")
            .with_file(r"C:\npm\claude.exe");
        let resolved = resolve_with(&sys, Agent::Claude, None, never_probed).unwrap();
        assert_eq!(resolved.origin, Origin::Path);
        assert_eq!(resolved.path, PathBuf::from(r"C:\npm\claude.exe"));
    }

    #[test]
    fn an_unpopulated_pathext_still_resolves_the_agent() {
        // `which` synthesises extensions only for a query that lacks one, and
        // falls back to the literal name when the list is empty — so the
        // explicit `.exe` in the query is what makes this survivable. Asserted
        // rather than assumed, because a query built without the suffix would
        // silently find nothing here and report the agent as not installed.
        let sys = FakeSys::windows()
            .with_env("PATH", r"C:\npm")
            .with_env("PATHEXT", "")
            .with_file(r"C:\npm\claude.exe");
        let resolved = resolve_with(&sys, Agent::Claude, None, never_probed).unwrap();
        assert_eq!(resolved.path, PathBuf::from(r"C:\npm\claude.exe"));
    }

    #[test]
    fn a_cmd_shim_on_path_is_not_accepted_as_the_agent() {
        // The npm shim is the only `claude` on PATH for most installs. Finding it
        // and spawning it would route every prompt through `cmd.exe`.
        let sys = FakeSys::windows()
            .with_env("PATH", r"C:\npm")
            .with_env("PATHEXT", ".COM;.EXE;.CMD")
            .with_file(r"C:\npm\claude.cmd");
        let err = resolve_with(&sys, Agent::Claude, None, never_probed).unwrap_err();
        assert!(matches!(err, ResolveError::NotFound("claude")));
    }

    #[test]
    fn the_npm_prefix_is_preferred_over_path() {
        let sys = FakeSys::windows()
            .with_env("NPM_CONFIG_PREFIX", r"C:\prefix")
            .with_env("PATH", r"C:\tools")
            .with_env("PATHEXT", ".EXE")
            .with_file(r"C:\tools\claude.exe")
            .with_file(r"C:\prefix\node_modules\@anthropic-ai\claude-code\bin\claude.exe");
        let resolved = resolve_with(&sys, Agent::Claude, None, never_probed).unwrap();
        assert_eq!(resolved.origin, Origin::NpmPrefix);
    }

    #[test]
    fn the_windows_npm_prefix_falls_back_to_appdata() {
        let sys = FakeSys::windows()
            .with_env("APPDATA", r"C:\Users\op\AppData\Roaming")
            .with_env("PATH", "")
            .with_file(
                r"C:\Users\op\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe",
            );
        let resolved = resolve_with(&sys, Agent::Claude, None, never_probed).unwrap();
        assert_eq!(resolved.origin, Origin::NpmPrefix);
    }

    #[test]
    fn a_missing_agent_is_reported_as_missing() {
        let sys = FakeSys::windows()
            .with_env("PATH", r"C:\tools")
            .with_env("PATHEXT", ".EXE")
            .with_dir(r"C:\tools");
        let err = resolve_with(&sys, Agent::Codex, None, never_probed).unwrap_err();
        assert!(matches!(err, ResolveError::NotFound("codex")));
    }

    #[test]
    fn a_configured_path_is_identified_against_the_injected_filesystem() {
        let sys = FakeSys::windows().with_file(r"C:\custom\claude.exe");
        let resolved = resolve_with(
            &sys,
            Agent::Claude,
            Some(Path::new(r"C:\custom\claude.exe")),
            |_| Ok("2.1.170 (Claude Code)".to_owned()),
        )
        .unwrap();
        assert_eq!(resolved.origin, Origin::Configured);
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
    fn the_npm_route_is_driven_by_the_manifest() {
        // The path used to be a match arm per agent. If the manifest's segments
        // stop resolving, the npm route must be skipped rather than guessed at —
        // a half-substituted path would silently fall through to PATH.
        for agent in [Agent::Claude, Agent::Codex] {
            let directory = agent
                .manifest()
                .npm_directory(Path::new("prefix"))
                .expect("this platform is covered by the manifest tables");
            assert!(!directory.to_string_lossy().contains('{'));
        }
    }
}
