//! Per-session environment leases declared by the repository.
//!
//! A worktree isolates files and nothing else. Two agents in two worktrees still
//! share ports, docker compose project names, databases, and every untracked
//! file that was never committed — which is the most-repeated unsolved complaint
//! in the community corpus, and the reason several people gave up on running
//! agents in parallel at all.
//!
//! Ports were already handled ([`crate::environment`]). This module adds the
//! three that actually bite, declared in `.terminalai/environment.toml` inside
//! the repository so the lease is versioned with the code it describes:
//!
//! - **untracked config**, copied in by glob — `.env`, local settings, anything
//!   the repo needs but never commits;
//! - **a docker compose project prefix**, so `docker compose up` in two sessions
//!   builds two stacks rather than fighting over one;
//! - **a database cloned from a template**, because "ten different copies of my
//!   database" is where hand-rolled isolation usually stops.
//!
//! Two deliberate limits. Depth over generality: these three stacks are handled
//! concretely rather than through a generic hook API, because a generic hook is
//! what every other tool already tells the operator to write themselves. And
//! nothing here is best-effort — a teardown that fails is reported, never
//! swallowed, since a lease that silently leaks is worse than one that was never
//! offered.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a repository declares its lease, relative to the repository root.
pub const LEASE_FILE: &str = ".terminalai/environment.toml";
/// Cap on files copied by one glob, so a mistaken `**/*` cannot copy a tree.
pub const MAX_COPIED_FILES: usize = 512;
/// Cap on one copied file, for the same reason.
pub const MAX_COPIED_BYTES: u64 = 8 * 1024 * 1024;

/// A repository's declared lease. Every section is optional: a repo that
/// declares only `[copy]` gets only file copying.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lease {
    /// Globs, relative to the repository root, of untracked files a session
    /// needs. Matched against the *source* repository, copied into the session's
    /// working directory.
    #[serde(default)]
    pub copy: Vec<String>,
    /// Docker compose isolation. The project name becomes
    /// `<prefix>-<session-id>`, which is what keeps two sessions' containers,
    /// networks and volumes apart.
    #[serde(default)]
    pub compose: Option<ComposeLease>,
    /// A database cloned per session from a template.
    #[serde(default)]
    pub database: Option<DatabaseLease>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComposeLease {
    /// Prefix for `COMPOSE_PROJECT_NAME`. Defaults to the repository folder.
    #[serde(default)]
    pub project_prefix: Option<String>,
    /// Compose file, relative to the repository root. Only used for teardown;
    /// starting the stack stays the operator's business.
    #[serde(default)]
    pub file: Option<String>,
    /// Whether teardown removes named volumes too. This is reserved for a
    /// trusted operator-side configuration; repository TOML cannot enable it.
    #[serde(default)]
    pub remove_volumes: bool,
}

/// Postgres, and only Postgres, on purpose.
///
/// `CREATE DATABASE ... TEMPLATE ...` is a genuine per-session clone rather than
/// a migration replay, and handling one engine properly is worth more than
/// handling four badly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseLease {
    /// The database cloned for each session.
    pub template: String,
    /// Prefix for the per-session database name. Defaults to the template.
    #[serde(default)]
    pub name_prefix: Option<String>,
    /// A libpq connection string for the *server*, not the session database.
    /// Read from this environment variable so a password never lands in a file
    /// that is committed with the repository. Only operator-owned
    /// `TERMINALAI_*` names are accepted here.
    #[serde(default = "default_admin_url_var")]
    pub admin_url_env: String,
    /// The variable the session's own database URL is exposed as. It may use
    /// the conventional `DATABASE_URL` name, but cannot shadow the sanitized
    /// process baseline or TerminalAI's own session variables.
    #[serde(default = "default_session_url_var")]
    pub session_url_env: String,
    /// Drop the session database on teardown.
    #[serde(default = "default_true")]
    pub drop_on_teardown: bool,
}

fn default_admin_url_var() -> String {
    "TERMINALAI_DB_ADMIN_URL".to_owned()
}

fn default_session_url_var() -> String {
    "DATABASE_URL".to_owned()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("{LEASE_FILE} is not valid TOML: {0}")]
    Parse(String),
    #[error("{LEASE_FILE} could not be read: {0}")]
    Read(String),
    #[error("{LEASE_FILE} declares an unusable value: {0}")]
    Invalid(String),
}

impl Lease {
    /// Read the lease a repository declares, if it declares one.
    ///
    /// A missing file is `Ok(None)` — most repositories will never have one. A
    /// malformed file is an error rather than an empty lease, because silently
    /// ignoring a lease the operator wrote is how sessions end up sharing a
    /// database while appearing isolated.
    pub fn load(repo_root: &Path) -> Result<Option<Lease>, LeaseError> {
        let path = repo_root.join(LEASE_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(LeaseError::Read(error.to_string())),
        };
        Ok(Some(Self::parse(&text)?))
    }

    /// Parse and validate. Validation lives here rather than in `load` so a
    /// caller that parses a lease from anywhere else cannot end up with one that
    /// escapes the repository or carries an unsafe database name.
    pub fn parse(text: &str) -> Result<Lease, LeaseError> {
        let document = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| LeaseError::Parse(error.to_string()))?;
        let mut lease = Lease::default();

        if let Some(copy) = document.get("copy").and_then(|item| item.as_array()) {
            for entry in copy {
                let glob = entry
                    .as_str()
                    .ok_or_else(|| LeaseError::Invalid("copy entries must be strings".into()))?;
                lease.copy.push(glob.to_owned());
            }
        }

        if let Some(compose) = document.get("compose").and_then(|item| item.as_table_like()) {
            lease.compose = Some(ComposeLease {
                project_prefix: string_field(compose, "project_prefix"),
                file: string_field(compose, "file"),
                // A repository may describe its own compose file, but it may
                // not authorize destructive volume removal during teardown.
                remove_volumes: false,
            });
        }

        if let Some(database) = document.get("database").and_then(|item| item.as_table_like()) {
            let template = string_field(database, "template").ok_or_else(|| {
                LeaseError::Invalid("[database] needs a template to clone from".into())
            })?;
            lease.database = Some(DatabaseLease {
                template,
                name_prefix: string_field(database, "name_prefix"),
                admin_url_env: string_field(database, "admin_url_env")
                    .unwrap_or_else(default_admin_url_var),
                session_url_env: string_field(database, "session_url_env")
                    .unwrap_or_else(default_session_url_var),
                drop_on_teardown: database
                    .get("drop_on_teardown")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true),
            });
        }

        lease.validate()?;
        Ok(lease)
    }

    fn validate(&self) -> Result<(), LeaseError> {
        for glob in &self.copy {
            validate_repository_relative_path("copy glob", glob)?;
        }
        if let Some(database) = &self.database {
            validate_identifier("database template", &database.template)?;
            if let Some(prefix) = &database.name_prefix {
                validate_identifier("database name_prefix", prefix)?;
            }
            validate_environment_variable(
                "database admin_url_env",
                &database.admin_url_env,
                true,
            )?;
            validate_environment_variable(
                "database session_url_env",
                &database.session_url_env,
                false,
            )?;
        }
        if let Some(compose) = &self.compose {
            if let Some(prefix) = &compose.project_prefix {
                validate_project_name("compose project_prefix", prefix)?;
            }
            if let Some(file) = &compose.file {
                validate_repository_relative_path("compose file", file)?;
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.copy.is_empty() && self.compose.is_none() && self.database.is_none()
    }
}

fn string_field(table: &dyn toml_edit::TableLike, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn validate_repository_relative_path(what: &str, value: &str) -> Result<(), LeaseError> {
    let drive_prefix = value.len() >= 2
        && value.as_bytes()[1] == b':'
        && value.as_bytes()[0].is_ascii_alphabetic();
    if value.is_empty()
        || value.chars().any(char::is_control)
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains("..")
        || drive_prefix
        || Path::new(value).is_absolute()
    {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} must stay inside the repository"
        )));
    }
    Ok(())
}

/// A SQL identifier this module is willing to interpolate.
///
/// `CREATE DATABASE` takes no bind parameters, so the name is concatenated into
/// the statement and the only defence is refusing anything that is not a plain
/// identifier. Deliberately stricter than Postgres allows.
fn validate_identifier(what: &str, value: &str) -> Result<(), LeaseError> {
    if value.is_empty() || value.len() > 48 {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} must be 1-48 characters"
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} may contain only letters, digits and underscores"
        )));
    }
    if value.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} must not start with a digit"
        )));
    }
    Ok(())
}

const RESERVED_SESSION_ENVIRONMENT_KEYS: &[&str] = &[
    "TERMINALAI_SESSION_ID",
    "TERMINALAI_PORTS",
    "TERMINALAI_PORT_BASE",
    "TERMINALAI_HOOK_TOKEN",
    "TERMINALAI_COMPOSE_PROJECT",
    "TERMINALAI_DB_NAME",
    "COMPOSE_PROJECT_NAME",
    "PORT",
];

/// Validate a repository-selected environment name before it reaches either
/// the daemon's lookup or the agent's child environment.
fn validate_environment_variable(
    what: &str,
    value: &str,
    require_operator_prefix: bool,
) -> Result<(), LeaseError> {
    validate_identifier(what, value)?;
    if require_operator_prefix && !value.starts_with("TERMINALAI_") {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} must use the operator-controlled TERMINALAI_ prefix"
        )));
    }
    if crate::environment::safe_environment_keys()
        .iter()
        .any(|key| value.eq_ignore_ascii_case(key))
    {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} may not shadow the sanitized process environment"
        )));
    }
    if RESERVED_SESSION_ENVIRONMENT_KEYS
        .iter()
        .any(|key| value.eq_ignore_ascii_case(key))
    {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} is reserved by TerminalAI"
        )));
    }
    Ok(())
}

/// Compose project names allow hyphens; everything else is as strict.
fn validate_project_name(what: &str, value: &str) -> Result<(), LeaseError> {
    if value.is_empty() || value.len() > 48 {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} must be 1-48 characters"
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(LeaseError::Invalid(format!(
            "{what} {value:?} may contain only lowercase letters, digits, hyphens and underscores"
        )));
    }
    Ok(())
}

/// A lease resolved for one specific session: every name already substituted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLease {
    pub session_id: String,
    pub copy: Vec<String>,
    pub compose_project: Option<String>,
    pub compose_file: Option<String>,
    pub compose_remove_volumes: bool,
    pub database: Option<ResolvedDatabase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDatabase {
    pub template: String,
    pub name: String,
    pub admin_url_env: String,
    pub session_url_env: String,
    pub drop_on_teardown: bool,
}

/// Session ids are `s0001`; the suffix is what makes two sessions' resources
/// distinguishable, and it is already safe for both SQL identifiers and compose
/// project names.
fn session_suffix(session_id: &str) -> String {
    let cleaned: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if cleaned.is_empty() {
        "session".to_owned()
    } else {
        cleaned.to_ascii_lowercase()
    }
}

fn canonicalize_with_missing_tail(path: &Path) -> std::io::Result<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
        })?;
    }

    let mut canonical = std::fs::canonicalize(existing)?;
    for name in missing.iter().rev() {
        canonical.push(name);
    }
    Ok(canonical)
}

fn canonical_compose_file(repo_root: &Path, file: &str) -> Result<String, LeaseError> {
    let canonical_root =
        std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let candidate = canonical_root.join(file);
    let canonical = canonicalize_with_missing_tail(&candidate).map_err(|error| {
        LeaseError::Invalid(format!(
            "compose file {file:?} could not be resolved under the repository: {error}"
        ))
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(LeaseError::Invalid(format!(
            "compose file {file:?} resolves outside the repository"
        )));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

impl Lease {
    /// Resolve this lease for one session.
    pub fn resolve(&self, session_id: &str, repo_root: &Path) -> Result<ResolvedLease, LeaseError> {
        self.validate()?;
        let suffix = session_suffix(session_id);
        let folder = repo_root
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .map(|name| {
                name.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>()
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "terminalai".to_owned());

        let compose_file = self
            .compose
            .as_ref()
            .and_then(|compose| compose.file.as_deref())
            .map(|file| canonical_compose_file(repo_root, file))
            .transpose()?;

        Ok(ResolvedLease {
            session_id: session_id.to_owned(),
            copy: self.copy.clone(),
            compose_project: self.compose.as_ref().map(|compose| {
                let prefix = compose.project_prefix.clone().unwrap_or(folder);
                format!("{prefix}-{suffix}")
            }),
            compose_file,
            compose_remove_volumes: self
                .compose
                .as_ref()
                .is_some_and(|compose| compose.remove_volumes),
            database: self.database.as_ref().map(|database| {
                let prefix = database
                    .name_prefix
                    .clone()
                    .unwrap_or_else(|| database.template.clone());
                ResolvedDatabase {
                    template: database.template.clone(),
                    name: format!("{prefix}_{suffix}"),
                    admin_url_env: database.admin_url_env.clone(),
                    session_url_env: database.session_url_env.clone(),
                    drop_on_teardown: database.drop_on_teardown,
                }
            }),
        })
    }
}

impl ResolvedLease {
    /// Environment variables this lease contributes to the session.
    ///
    /// `admin_url` is the operator's server connection string, read from the
    /// process environment; it is used to derive the session database URL and is
    /// never itself exported to the agent.
    pub fn variables(&self, admin_url: Option<&str>) -> Vec<(String, String)> {
        let mut values = Vec::new();
        if let Some(project) = &self.compose_project {
            values.push(("COMPOSE_PROJECT_NAME".to_owned(), project.clone()));
            values.push(("TERMINALAI_COMPOSE_PROJECT".to_owned(), project.clone()));
        }
        if let Some(database) = &self.database {
            values.push(("TERMINALAI_DB_NAME".to_owned(), database.name.clone()));
            if let Some(admin) = admin_url {
                if let Some(url) = session_database_url(admin, &database.name) {
                    values.push((database.session_url_env.clone(), url));
                }
            }
        }
        values
    }

    /// The `psql` argument vector that creates this session's database.
    ///
    /// Returned as data rather than run here so the exact statement is testable
    /// without a live server.
    pub fn create_database_args(&self) -> Option<Vec<String>> {
        let database = self.database.as_ref()?;
        Some(vec![
            "--no-psqlrc".to_owned(),
            "--quiet".to_owned(),
            // Refuse to continue past a failed statement: a half-provisioned
            // database that looks ready is the failure mode being avoided.
            "--set".to_owned(),
            "ON_ERROR_STOP=1".to_owned(),
            "--command".to_owned(),
            format!(
                "CREATE DATABASE \"{}\" TEMPLATE \"{}\"",
                database.name, database.template
            ),
        ])
    }

    /// The `psql` argument vector that drops it.
    pub fn drop_database_args(&self) -> Option<Vec<String>> {
        let database = self.database.as_ref()?;
        if !database.drop_on_teardown {
            return None;
        }
        Some(vec![
            "--no-psqlrc".to_owned(),
            "--quiet".to_owned(),
            "--set".to_owned(),
            "ON_ERROR_STOP=1".to_owned(),
            "--command".to_owned(),
            // WITH (FORCE) disconnects anything the agent left connected;
            // without it a lingering client makes teardown fail every time.
            format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", database.name),
        ])
    }

    /// The `docker` argument vector that removes this session's compose stack.
    pub fn compose_down_args(&self) -> Option<Vec<String>> {
        let project = self.compose_project.as_ref()?;
        let mut args = vec!["compose".to_owned(), "--project-name".to_owned(), project.clone()];
        if let Some(file) = &self.compose_file {
            args.push("--file".to_owned());
            args.push(file.clone());
        }
        args.push("down".to_owned());
        args.push("--remove-orphans".to_owned());
        if self.compose_remove_volumes {
            args.push("--volumes".to_owned());
        }
        Some(args)
    }
}

/// Swap the database component of a libpq URL for this session's database.
///
/// Returns `None` for anything that is not a recognisable URL rather than
/// guessing — a malformed connection string that silently became a valid-looking
/// one would point the session at the wrong database.
pub fn session_database_url(admin_url: &str, database: &str) -> Option<String> {
    let admin_url = admin_url.trim();
    let (scheme, rest) = admin_url.split_once("://")?;
    if !matches!(scheme, "postgres" | "postgresql") {
        return None;
    }
    // Split off any query string first: it may contain slashes.
    let (authority_and_path, query) = match rest.split_once('?') {
        Some((head, tail)) => (head, Some(tail)),
        None => (rest, None),
    };
    let authority = authority_and_path
        .split_once('/')
        .map(|(authority, _)| authority)
        .unwrap_or(authority_and_path);
    if authority.is_empty() {
        return None;
    }
    let mut url = format!("{scheme}://{authority}/{database}");
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    Some(url)
}

/// Files a lease's globs select, relative to `repo_root`.
///
/// Matching is done here rather than with a glob crate because the supported
/// syntax is deliberately small: `*` and `?` within one path segment, and a
/// leading `**/` to mean "at any depth". Anything richer invites a glob that
/// walks the whole tree.
pub fn matching_files(repo_root: &Path, globs: &[String]) -> Result<Vec<PathBuf>, LeaseError> {
    let mut matches: BTreeMap<PathBuf, ()> = BTreeMap::new();
    for glob in globs {
        let (recursive, pattern) = match glob.strip_prefix("**/") {
            Some(rest) => (true, rest),
            None => (false, glob.as_str()),
        };
        let (directory, file_pattern) = match pattern.rsplit_once('/') {
            Some((directory, file)) => (repo_root.join(directory), file),
            None => (repo_root.to_path_buf(), pattern),
        };
        if recursive {
            collect_recursive(repo_root, &directory, file_pattern, &mut matches)?;
        } else {
            collect_in(&directory, file_pattern, &mut matches)?;
        }
        if matches.len() > MAX_COPIED_FILES {
            return Err(LeaseError::Invalid(format!(
                "copy globs select more than {MAX_COPIED_FILES} files; narrow them"
            )));
        }
    }
    Ok(matches.into_keys().collect())
}

fn collect_in(
    directory: &Path,
    pattern: &str,
    matches: &mut BTreeMap<PathBuf, ()>,
) -> Result<(), LeaseError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        // A glob pointing at a directory that does not exist is not an error:
        // an optional config file is the normal case.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(LeaseError::Read(error.to_string())),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !glob_matches(pattern, &name) {
            continue;
        }
        if entry.path().is_file() {
            matches.insert(entry.path(), ());
        }
    }
    Ok(())
}

fn collect_recursive(
    repo_root: &Path,
    directory: &Path,
    pattern: &str,
    matches: &mut BTreeMap<PathBuf, ()>,
) -> Result<(), LeaseError> {
    // Bounded walk: `.git`, `node_modules` and `target` are never config, and
    // walking them is what makes a `**/` glob feel like a hang.
    const SKIP: [&str; 5] = [".git", "node_modules", "target", "dist", ".venv"];
    let mut queue = vec![directory.to_path_buf()];
    let mut visited = 0usize;
    while let Some(current) = queue.pop() {
        visited += 1;
        if visited > 4096 {
            return Err(LeaseError::Invalid(
                "a recursive copy glob walked too many directories; narrow it".into(),
            ));
        }
        collect_in(&current, pattern, matches)?;
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP.contains(&name.as_ref()) || name.starts_with('.') && name != ".terminalai" {
                continue;
            }
            queue.push(path);
        }
    }
    let _ = repo_root;
    Ok(())
}

/// `*` and `?` within one path segment. No `**` here — that is handled by the
/// caller as a directory walk.
fn glob_matches(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[u8], name: &[u8]) -> bool {
        match pattern.first() {
            None => name.is_empty(),
            Some(b'*') => {
                // Match the rest here, or consume one more character.
                matches(&pattern[1..], name)
                    || (!name.is_empty() && matches(pattern, &name[1..]))
            }
            Some(b'?') => !name.is_empty() && matches(&pattern[1..], &name[1..]),
            Some(expected) => {
                !name.is_empty()
                    && name[0].eq_ignore_ascii_case(expected)
                    && matches(&pattern[1..], &name[1..])
            }
        }
    }
    matches(pattern.as_bytes(), name.as_bytes())
}

/// Copy the files a lease selects from `source` into `destination`.
///
/// Returns the relative paths copied. An existing file in the destination is
/// left alone: the session may have already edited it, and overwriting an
/// agent's work to satisfy a lease would be its own failure.
pub fn copy_files(
    source: &Path,
    destination: &Path,
    globs: &[String],
) -> Result<Vec<PathBuf>, LeaseError> {
    if source == destination {
        return Ok(Vec::new());
    }
    let mut copied = Vec::new();
    for path in matching_files(source, globs)? {
        let relative = path.strip_prefix(source).unwrap_or(&path).to_path_buf();
        let target = destination.join(&relative);
        if target.exists() {
            continue;
        }
        let size = std::fs::metadata(&path)
            .map_err(|error| LeaseError::Read(error.to_string()))?
            .len();
        if size > MAX_COPIED_BYTES {
            return Err(LeaseError::Invalid(format!(
                "{} is larger than the {MAX_COPIED_BYTES}-byte copy limit",
                relative.display()
            )));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| LeaseError::Read(error.to_string()))?;
        }
        std::fs::copy(&path, &target).map_err(|error| LeaseError::Read(error.to_string()))?;
        copied.push(relative);
    }
    Ok(copied)
}
