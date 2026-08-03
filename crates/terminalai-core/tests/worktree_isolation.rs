//! Per-session worktrees, exercised against real Git repositories.
//!
//! The thing being tested is that two agents on one repository cannot see each
//! other's uncommitted edits, and that cleaning up never destroys work. Neither
//! is provable against a fake: both are statements about what Git and the
//! filesystem actually did.

use std::path::{Path, PathBuf};
use std::process::Command;

use terminalai_core::worktree::{self, WorktreeError};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} in {}: {error}", repo.display()));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "terminalai-worktree-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    Scratch(dir)
}

/// A repository with one commit, plus the directory worktrees are cut into.
fn repo(name: &str) -> (Scratch, PathBuf, PathBuf) {
    let root = scratch(name);
    let repo = root.0.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "user.name", "Worktree Test"]);
    std::fs::write(repo.join("file.txt"), "committed\n").expect("seed");
    git(&repo, &["add", "file.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "seed"]);
    let worktrees = root.0.join("worktrees");
    (root, repo, worktrees)
}

#[test]
fn two_sessions_on_one_repository_cannot_see_each_other_s_edits() {
    // The failure this exists to remove. Without isolation both agents share
    // one working tree, and one agent's half-finished edit becomes the other's
    // unexplained diff.
    let (_root, repo, worktrees) = repo("isolated");
    let first = worktree::create(&worktrees, &repo, "s0001").expect("first worktree");
    let second = worktree::create(&worktrees, &repo, "s0002").expect("second worktree");

    std::fs::write(first.path.join("file.txt"), "edited by the first session\n").expect("edit");

    let seen_by_second = std::fs::read_to_string(second.path.join("file.txt")).expect("read");
    assert_eq!(seen_by_second, "committed\n");
    let seen_by_repo = std::fs::read_to_string(repo.join("file.txt")).expect("read");
    assert_eq!(seen_by_repo, "committed\n");
    assert_ne!(first.path, second.path);
    assert_ne!(first.branch, second.branch);
}

#[test]
fn a_session_launched_from_a_subdirectory_still_gets_the_whole_repository() {
    // Launching from `repo/crates/foo` is normal. Cutting a worktree from the
    // subdirectory rather than the repository root would fail, or worse, look
    // like it worked and check out the wrong thing.
    let (_root, repo, worktrees) = repo("subdir");
    let nested = repo.join("crates").join("inner");
    std::fs::create_dir_all(&nested).expect("nested");

    let created = worktree::create(&worktrees, &nested, "s0001").expect("worktree");
    assert!(created.path.join("file.txt").exists(), "not a full checkout");
    assert_eq!(
        std::fs::canonicalize(&created.repo).expect("canonical"),
        std::fs::canonicalize(&repo).expect("canonical"),
    );
}

#[test]
fn a_directory_that_already_exists_is_refused_rather_than_checked_out_over() {
    // It may hold work from an earlier run that nobody has landed. This code
    // cannot tell that from litter, so it does not guess.
    let (_root, repo, worktrees) = repo("occupied");
    let occupied = worktree::path_for(&worktrees, &repo, "s0001");
    std::fs::create_dir_all(&occupied).expect("occupy");
    std::fs::write(occupied.join("unlanded.txt"), "someone's work").expect("write");

    let error = worktree::create(&worktrees, &repo, "s0001").expect_err("must refuse");
    assert!(matches!(error, WorktreeError::PathExists(_)), "{error:?}");
    assert!(occupied.join("unlanded.txt").exists(), "refusal destroyed work");
}

#[test]
fn an_existing_branch_is_refused_rather_than_adopted() {
    // Adopting it would silently place this session on another session's work.
    let (_root, repo, worktrees) = repo("branch-taken");
    let branch = worktree::branch_for("s0001");
    git(&repo, &["branch", &branch]);

    let error = worktree::create(&worktrees, &repo, "s0001").expect_err("must refuse");
    assert!(matches!(error, WorktreeError::BranchExists(_)), "{error:?}");
}

#[test]
fn a_directory_outside_any_repository_is_refused() {
    let (root, _repo, worktrees) = repo("not-a-repo");
    let outside = root.0.join("plain");
    std::fs::create_dir_all(&outside).expect("dir");
    assert!(worktree::create(&worktrees, &outside, "s0001").is_err());
}

#[test]
fn removing_a_worktree_takes_the_checkout_and_leaves_the_repository_intact() {
    let (_root, repo, worktrees) = repo("remove");
    let created = worktree::create(&worktrees, &repo, "s0001").expect("worktree");
    assert!(created.path.exists());

    let failures = worktree::remove(&created);
    assert!(failures.is_empty(), "{failures:?}");
    assert!(!created.path.exists(), "checkout survived");
    assert!(repo.join("file.txt").exists(), "removal touched the repository");
    // The registration must go with it, or the next `worktree add` for this
    // path fails on a directory git still believes in.
    let listed = git(&repo, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains("s0001"), "{listed}");
}

#[test]
fn removal_reports_a_branch_holding_unmerged_work_instead_of_deleting_it() {
    // `git branch -D` here would destroy the session's commits. The whole
    // point of the worktree is that the work exists; losing it on cleanup
    // would be worse than never isolating at all.
    let (_root, repo, worktrees) = repo("unmerged");
    let created = worktree::create(&worktrees, &repo, "s0001").expect("worktree");
    std::fs::write(created.path.join("file.txt"), "session work\n").expect("edit");
    git(&created.path, &["add", "file.txt"]);
    git(&created.path, &["commit", "--quiet", "-m", "session work"]);

    let failures = worktree::remove(&created);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("not merged"), "{failures:?}");
    // The commit is still reachable from the branch that was kept.
    let log = git(&repo, &["log", "--oneline", &created.branch]);
    assert!(log.contains("session work"), "{log}");
}

#[test]
fn a_checkout_deleted_behind_git_s_back_is_repaired_rather_than_orphaned() {
    // Git refuses to remove a worktree whose directory is gone and keeps the
    // registration forever, so every later add for that path fails. Observed
    // before as an exit-7 loop that no amount of retrying cleared.
    let (_root, repo, worktrees) = repo("orphan");
    let created = worktree::create(&worktrees, &repo, "s0001").expect("worktree");
    std::fs::remove_dir_all(&created.path).expect("simulate a vanished directory");

    let failures = worktree::remove(&created);
    assert!(failures.is_empty(), "{failures:?}");
    let listed = git(&repo, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains("s0001"), "registration was orphaned: {listed}");

    // The proof that the repair worked: the same path can be used again.
    let again = worktree::create(&worktrees, &repo, "s0001");
    assert!(again.is_ok(), "{:?}", again.err());
}
