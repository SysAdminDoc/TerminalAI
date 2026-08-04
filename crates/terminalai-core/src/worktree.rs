//! A private checkout per session.
//!
//! Two agents editing one working tree is the failure this removes. It is not a
//! hypothetical: the fleet's whole premise is many sessions at once, and the
//! obvious way to use it — several agents on one repository — is exactly the
//! case where one agent's uncommitted edit becomes another's mysterious diff.
//!
//! A worktree gives each session its own files and its own branch while sharing
//! one object database, so it costs a checkout rather than a clone.
//!
//! The rules here are all about not destroying work:
//!
//! - **Nothing is reused.** A branch or directory that already exists is a
//!   refusal, never an adoption — the alternative is a session silently
//!   inheriting another one's state.
//! - **Removal never deletes commits.** The checkout goes; the branch is offered
//!   to `git branch -d`, which refuses when it holds work that is not merged,
//!   and a refusal there is reported rather than forced.
//! - **A failed removal is repaired, not ignored.** `git worktree remove` fails
//!   if the directory is gone from under it, leaving a registration that makes
//!   every later `git worktree add` complain about a path that does not exist.
//!   The fallback deletes the directory and prunes.
//!
//! The git half of session isolation only. Ports, services and databases are
//! the other half, and live in [`crate::lease`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::land::{run_program, ProcessRun};

/// Ceiling for one git call. A worktree add copies a checkout, so this is more
/// generous than a status query would need, and still bounded: the launch path
/// is holding a session in `Starting` while this runs.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Branch names are prefixed so the operator can tell at a glance which branches
/// this tool created, and so `git branch --list terminalai/*` cleans up.
pub const BRANCH_PREFIX: &str = "terminalai/";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorktreeError {
    #[error("{0} is not inside a Git repository, so a per-session worktree cannot be created")]
    NotARepository(PathBuf),
    #[error("branch {0} already exists; refusing to reuse another session's branch")]
    BranchExists(String),
    #[error("{0} already exists; refusing to check out over it")]
    PathExists(PathBuf),
    #[error("{0} is a bare repository and has no files to copy into a worktree")]
    BareRepository(PathBuf),
    #[error("git {command} failed: {detail}")]
    Git { command: String, detail: String },
}

/// A checkout created for one session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    /// The main repository this was cut from — where a landing goes back to.
    pub repo: PathBuf,
    /// The session's own directory. This becomes its working directory.
    pub path: PathBuf,
    pub branch: String,
}

/// The branch a session gets. Derived from the id, so it is stable across a
/// restart and unique without a counter.
pub fn branch_for(session_id: &str) -> String {
    let mut name = String::with_capacity(BRANCH_PREFIX.len() + session_id.len());
    name.push_str(BRANCH_PREFIX);
    // Git refs reject a range of characters and sequences. Rather than encode
    // the whole of `git check-ref-format`, keep an allowlist: session ids are
    // ours (`s0001`), so anything outside it is already unexpected.
    for byte in session_id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            name.push(byte as char);
        } else {
            name.push('-');
        }
    }
    if name == BRANCH_PREFIX {
        name.push_str("session");
    }
    name
}

/// Where a session's checkout goes, given the directory the daemon owns.
///
/// Outside the repository on purpose. A worktree inside it shows up in the
/// parent's `git status` as an untracked directory, gets walked by every build
/// tool and test runner in the tree, and turns one accidental `rm -rf` into the
/// loss of every session's work at once.
pub fn path_for(root: &Path, repo: &Path, session_id: &str) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    let mut directory = String::new();
    for byte in format!("{repo_name}-{session_id}").bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            directory.push(byte as char);
        } else {
            directory.push('_');
        }
    }
    root.join(directory)
}

/// The repository root containing `cwd`, if there is one.
///
/// Resolved rather than assumed: a session is commonly launched from a
/// subdirectory, and cutting a worktree from `repo/crates/foo` would otherwise
/// fail with a message about the wrong path.
pub fn repository_root(cwd: &Path) -> Result<PathBuf, WorktreeError> {
    let inside = git(cwd, &["rev-parse", "--is-inside-work-tree"])?;
    if inside.trim() != "true" {
        return Err(WorktreeError::BareRepository(cwd.to_path_buf()));
    }
    let root = git(cwd, &["rev-parse", "--show-toplevel"])?;
    let root = root.trim();
    if root.is_empty() {
        return Err(WorktreeError::NotARepository(cwd.to_path_buf()));
    }
    Ok(PathBuf::from(root))
}

/// Create a session's worktree on a new branch off the current HEAD.
pub fn create(root: &Path, cwd: &Path, session_id: &str) -> Result<Worktree, WorktreeError> {
    let repo = repository_root(cwd)?;
    let branch = branch_for(session_id);
    let path = path_for(root, &repo, session_id);

    // Both checks are refusals rather than cleanups. A directory or branch left
    // behind by an earlier run may hold work nobody has landed yet, and this
    // code cannot tell the difference between that and litter.
    if path.exists() {
        return Err(WorktreeError::PathExists(path));
    }
    if branch_exists(&repo, &branch) {
        return Err(WorktreeError::BranchExists(branch));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| WorktreeError::Git {
            command: "worktree add".to_owned(),
            detail: format!("could not create {}: {error}", parent.display()),
        })?;
    }

    let path_text = path.to_string_lossy().into_owned();
    git(
        &repo,
        &["worktree", "add", "-b", &branch, &path_text, "HEAD"],
    )?;
    Ok(Worktree { repo, path, branch })
}

/// Remove a session's worktree, returning every failure rather than the first.
///
/// A caller reporting "could not clean up" needs to know whether the checkout
/// survived, the branch survived, or both — they are repaired differently.
pub fn remove(worktree: &Worktree) -> Vec<String> {
    let mut failures = Vec::new();
    let path_text = worktree.path.to_string_lossy().into_owned();

    let removed = git(
        &worktree.repo,
        &["worktree", "remove", "--force", &path_text],
    );
    if let Err(error) = removed {
        // The usual cause is that the directory is already gone — a removed
        // drive, or an operator who deleted it. Git then keeps the registration
        // forever, and every later `worktree add` for that path fails. Deleting
        // what is left and pruning is the documented repair.
        let _ = std::fs::remove_dir_all(&worktree.path);
        if let Err(prune) = git(&worktree.repo, &["worktree", "prune"]) {
            failures.push(format!("{error}; and the repair also failed: {prune}"));
        } else if worktree.path.exists() {
            failures.push(format!("{error}; {} still exists", worktree.path.display()));
        }
    }

    // Never `-D`. A branch git refuses to delete is holding commits that are
    // not merged anywhere — that is the session's work, and losing it silently
    // would be far worse than leaving a branch behind.
    if branch_exists(&worktree.repo, &worktree.branch) {
        if let Err(error) = git(&worktree.repo, &["branch", "-d", &worktree.branch]) {
            failures.push(format!(
                "branch {} was kept because it holds work that is not merged ({error})",
                worktree.branch
            ));
        }
    }
    failures
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    let reference = format!("refs/heads/{branch}");
    matches!(
        run_program(
            "git",
            repo,
            &["show-ref", "--verify", "--quiet", &reference],
            None,
            GIT_TIMEOUT,
        ),
        ProcessRun::Completed {
            status: Some(0),
            ..
        }
    )
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    match run_program("git", cwd, args, None, GIT_TIMEOUT) {
        ProcessRun::Completed {
            status: Some(0),
            stdout,
            ..
        } => Ok(stdout),
        ProcessRun::Completed { stderr, .. } => Err(WorktreeError::Git {
            command: args.join(" "),
            detail: first_line(&stderr),
        }),
        ProcessRun::TimedOut => Err(WorktreeError::Git {
            command: args.join(" "),
            detail: format!("did not finish within {GIT_TIMEOUT:?}"),
        }),
        ProcessRun::Failed { detail } => Err(WorktreeError::Git {
            command: args.join(" "),
            detail,
        }),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_name_is_derived_from_the_session_and_is_marked_as_ours() {
        assert_eq!(branch_for("s0001"), "terminalai/s0001");
        // An id carrying anything git would reject cannot produce an invalid
        // ref; it is mapped, not passed through.
        let hostile = branch_for("../../head~1");
        assert!(hostile.starts_with(BRANCH_PREFIX));
        assert!(!hostile[BRANCH_PREFIX.len()..].contains('.'));
        assert!(!hostile.contains('~') && !hostile.contains('/') || hostile.matches('/').count() == 1);
    }

    #[test]
    fn an_empty_session_id_still_produces_a_usable_branch() {
        assert_eq!(branch_for(""), "terminalai/session");
    }

    #[test]
    fn a_checkout_never_lands_inside_the_repository_it_came_from() {
        // Inside the repo it would be untracked clutter in the parent's status,
        // walked by every build tool, and lost with one careless delete.
        let root = Path::new(r"C:\data\TerminalAI\worktrees");
        let repo = Path::new(r"C:\repos\shop");
        let path = path_for(root, repo, "s0001");
        assert!(path.starts_with(root));
        assert!(!path.starts_with(repo));
        assert_eq!(path.file_name().unwrap().to_string_lossy(), "shop-s0001");
    }

    #[test]
    fn two_sessions_on_one_repository_get_different_directories() {
        let root = Path::new(r"C:\data");
        let repo = Path::new(r"C:\repos\shop");
        assert_ne!(path_for(root, repo, "s0001"), path_for(root, repo, "s0002"));
        assert_ne!(branch_for("s0001"), branch_for("s0002"));
    }

    #[test]
    fn a_repository_name_never_escapes_the_worktree_root() {
        let root = Path::new(r"C:\data");
        let path = path_for(root, Path::new(r"C:\repos\..\..\evil"), "s0001");
        assert_eq!(path.parent().unwrap(), root);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains(".."), "{name}");
    }

    fn stale(state: BranchState) -> StaleWorktree {
        StaleWorktree {
            path: PathBuf::from(r"C:\data\worktrees\shop-s0001"),
            repo: PathBuf::from("C:/repos/shop"),
            branch: "terminalai/s0001".to_owned(),
            state,
            missing_directory: false,
        }
    }

    #[test]
    fn only_a_fully_merged_branch_is_offered_for_removal() {
        assert!(stale(BranchState::Merged).is_safe_to_remove());
        assert!(!stale(BranchState::Unmerged { commits: 3 }).is_safe_to_remove());
    }

    #[test]
    fn an_unknown_state_is_never_treated_as_merged() {
        // "We could not tell" resolving to "delete it" is the one mistake in
        // this program that cannot be undone.
        let unknown = stale(BranchState::Unknown {
            detail: "git timed out".to_owned(),
        });
        assert!(!unknown.is_safe_to_remove());
        let refusal = reap(&unknown).expect_err("an unknown state must refuse");
        assert!(refusal[0].contains("unknown"), "{refusal:?}");
    }

    #[test]
    fn reaping_refuses_unmerged_work_in_the_core_not_only_in_the_window() {
        // A caller that skipped the check would otherwise delete commits, so the
        // refusal lives where every caller reaches it.
        let refusal = reap(&stale(BranchState::Unmerged { commits: 2 }))
            .expect_err("unmerged work must refuse");
        assert!(refusal[0].contains("2 commit"), "{refusal:?}");
        assert!(refusal[0].contains("terminalai/s0001"), "{refusal:?}");
    }

    #[test]
    fn a_missing_worktree_root_surveys_to_nothing_rather_than_failing() {
        let missing = Path::new(r"C:\data\definitely-not-here-9f3a");
        assert!(survey(missing, &[]).is_empty());
    }

    #[test]
    fn a_live_session_s_checkout_is_never_reported_as_stale() {
        let root = std::env::temp_dir().join("terminalai-survey-live");
        let owned = root.join("shop-s0001");
        std::fs::create_dir_all(&owned).expect("create fixture");
        let live = vec![Worktree {
            repo: PathBuf::from("C:/repos/shop"),
            path: owned.clone(),
            branch: "terminalai/s0001".to_owned(),
        }];
        assert!(survey(&root, &live).is_empty());
        // And with nothing live it is still not reported, because it is not a
        // git worktree at all — the survey inspects rather than assumes.
        assert!(survey(&root, &[]).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// What a leftover checkout is holding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum BranchState {
    /// Every commit on the branch is reachable from the repository's HEAD, so
    /// removing it loses nothing.
    Merged,
    /// The branch holds commits HEAD cannot reach. This is somebody's work.
    Unmerged { commits: u32 },
    /// The question could not be answered — an unreadable repository, a missing
    /// branch, a git that timed out. Never treated as merged: the whole point of
    /// this survey is that deleting on a guess is the one unrecoverable mistake.
    Unknown { detail: String },
}

/// A checkout this tool created that no live session owns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StaleWorktree {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub branch: String,
    pub state: BranchState,
    /// True when the directory is gone but git still has the registration —
    /// the case that makes every later `worktree add` for that path fail.
    pub missing_directory: bool,
}

impl StaleWorktree {
    /// Whether reaping this one can be offered without asking anything.
    pub fn is_safe_to_remove(&self) -> bool {
        matches!(self.state, BranchState::Merged)
    }
}

/// Every checkout under `root` that no live session owns.
///
/// Teardown deliberately keeps a branch holding unmerged work, which is right —
/// but nothing ever revisited it, so worktrees and branches accumulated silently
/// and their registrations outlived the directories. This is the revisit.
///
/// `live` is the set of worktrees the registry still owns; anything under the
/// root that is not in it is stale by definition, because the root belongs to
/// this tool alone.
pub fn survey(root: &Path, live: &[Worktree]) -> Vec<StaleWorktree> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let owned: std::collections::BTreeSet<&Path> = live.iter().map(|item| item.path.as_path()).collect();
    let mut stale = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || owned.contains(path.as_path()) {
            continue;
        }
        if let Some(found) = inspect(&path) {
            stale.push(found);
        }
    }
    stale.sort_by(|a, b| a.path.cmp(&b.path));
    stale
}

/// Read one leftover directory: which repository it came from, which branch it
/// is on, and whether that branch still holds work.
fn inspect(path: &Path) -> Option<StaleWorktree> {
    let repo = git(path, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .ok()
        .map(|dir| PathBuf::from(dir.trim()))
        .and_then(|git_dir| git_dir.parent().map(Path::to_path_buf))?;
    let branch = git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name != "HEAD")?;
    // Only branches this tool created. A worktree an operator put under the same
    // root by hand is left alone rather than offered for deletion.
    if !branch.starts_with(BRANCH_PREFIX) {
        return None;
    }
    Some(StaleWorktree {
        state: branch_state(&repo, &branch),
        missing_directory: !path.exists(),
        path: path.to_path_buf(),
        repo,
        branch,
    })
}

/// Commits on `branch` that the repository's HEAD cannot reach.
///
/// `rev-list --count <branch> --not HEAD` answers exactly the question that
/// matters — not "was it merged into the branch it was cut from", which goes
/// wrong the moment the operator rebases or renames.
fn branch_state(repo: &Path, branch: &str) -> BranchState {
    if !branch_exists(repo, branch) {
        return BranchState::Unknown {
            detail: format!("branch {branch} no longer exists in {}", repo.display()),
        };
    }
    match git(repo, &["rev-list", "--count", branch, "--not", "HEAD"]) {
        Ok(count) => match count.trim().parse::<u32>() {
            Ok(0) => BranchState::Merged,
            Ok(commits) => BranchState::Unmerged { commits },
            Err(error) => BranchState::Unknown {
                detail: format!("could not read the commit count: {error}"),
            },
        },
        Err(error) => BranchState::Unknown {
            detail: error.to_string(),
        },
    }
}

/// Remove one surveyed worktree, refusing anything that still holds work.
///
/// The refusal is here rather than only in the window: a caller that skipped the
/// check would otherwise delete commits, and this is the one mistake in the whole
/// program that cannot be undone. `Unknown` is refused for the same reason —
/// "we could not tell" must not resolve to "delete it".
pub fn reap(stale: &StaleWorktree) -> Result<(), Vec<String>> {
    if !stale.is_safe_to_remove() {
        return Err(vec![match &stale.state {
            BranchState::Unmerged { commits } => format!(
                "{} holds {commits} commit(s) that {} cannot reach; remove it with git if you mean to lose them",
                stale.branch,
                stale.repo.display()
            ),
            BranchState::Unknown { detail } => {
                format!("{} was not removed because its state is unknown: {detail}", stale.branch)
            }
            BranchState::Merged => unreachable!("checked above"),
        }]);
    }
    let failures = remove(&Worktree {
        repo: stale.repo.clone(),
        path: stale.path.clone(),
        branch: stale.branch.clone(),
    });
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}
