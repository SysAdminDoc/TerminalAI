//! The land gate, exercised against real Git repositories.
//!
//! Every refusal here is one an operator would otherwise discover by finding a
//! half-applied change in their tree, so these run real `git` rather than a
//! fake: the guarantee being tested is "the target is byte-for-byte unchanged",
//! which only the filesystem can confirm.

use std::path::{Path, PathBuf};
use std::process::Command;

use terminalai_core::land::{LandOutcome, LandQueue, LandRefusal, LandRequest};

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

/// A scratch directory that removes itself, so a failed test cannot leave a
/// repository behind for the next run to trip over.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "terminalai-land-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    Scratch(dir)
}

/// A repository with one committed file, plus a clone of it. The clone stands in
/// for the session's worktree; the original is the landing target.
fn repo_pair(name: &str, contents: &str) -> (Scratch, PathBuf, PathBuf) {
    let root = scratch(name);
    let target = root.0.join("target");
    std::fs::create_dir_all(&target).expect("target dir");
    git(&target, &["init", "--quiet", "--initial-branch=main"]);
    // Windows Git checks out CRLF by default; the assertions here compare exact
    // bytes, so the fixture pins line endings rather than the product changing.
    git(&target, &["config", "core.autocrlf", "false"]);
    git(&target, &["config", "user.email", "test@example.invalid"]);
    git(&target, &["config", "user.name", "Land Gate Test"]);
    std::fs::write(target.join("file.txt"), contents).expect("seed file");
    git(&target, &["add", "."]);
    git(&target, &["commit", "--quiet", "-m", "seed"]);

    let source = root.0.join("source");
    git(
        &root.0,
        &[
            // Set on the clone itself, not afterwards: a worktree checked out
            // with CRLF and then told LF reads as a full-file diff, which would
            // make an unchanged session look like it had work to land.
            "-c",
            "core.autocrlf=false",
            "clone",
            "--quiet",
            target.to_str().expect("utf8 target"),
            source.to_str().expect("utf8 source"),
        ],
    );
    git(&source, &["config", "core.autocrlf", "false"]);
    git(&source, &["config", "user.email", "test@example.invalid"]);
    git(&source, &["config", "user.name", "Land Gate Test"]);
    (root, target, source)
}

fn request(source: &Path, target: &Path) -> LandRequest {
    LandRequest {
        session: None,
        archive_on_success: false,
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        expected_target_head: None,
        verify: Vec::new(),
        verify_timeout_secs: None,
    }
}

fn refusal(outcome: LandOutcome) -> LandRefusal {
    match outcome {
        LandOutcome::Refused(refusal) => refusal,
        LandOutcome::Landed { .. } => panic!("expected a refusal, the landing went through"),
    }
}

#[test]
fn a_clean_change_lands_whole() {
    let (_root, target, source) = repo_pair("clean", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");

    let outcome = LandQueue::new().land(&request(&source, &target));
    match outcome {
        LandOutcome::Landed {
            files_changed,
            verified,
            ..
        } => {
            assert_eq!(files_changed, 1);
            // No verify command was configured, so this is recorded as absent
            // rather than reported as a pass.
            assert_eq!(verified, None);
        }
        LandOutcome::Refused(refusal) => panic!("expected a landing, got {refusal}"),
    }
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\ntwo\n"
    );
    // The change is left uncommitted: committing is the operator's decision.
    assert!(!git(&target, &["status", "--porcelain"]).trim().is_empty());
}

#[test]
fn a_target_that_moved_since_review_is_refused() {
    let (_root, target, source) = repo_pair("moved", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");
    let reviewed_head = git(&target, &["rev-parse", "HEAD"]).trim().to_owned();

    // Somebody else lands first — exactly what the serialised queue exists for.
    std::fs::write(target.join("other.txt"), "elsewhere\n").expect("other file");
    git(&target, &["add", "."]);
    git(&target, &["commit", "--quiet", "-m", "someone else landed"]);

    let mut plan = request(&source, &target);
    plan.expected_target_head = Some(reviewed_head.clone());
    match refusal(LandQueue::new().land(&plan)) {
        LandRefusal::TargetMoved { expected, found } => {
            assert_eq!(expected, reviewed_head);
            assert_ne!(found, reviewed_head);
        }
        other => panic!("expected TargetMoved, got {other}"),
    }
    // Nothing was written.
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\n"
    );
}

#[test]
fn an_abbreviated_reviewed_hash_still_matches_the_full_one() {
    // The review surface shows a short hash; refusing it would make the pin
    // unusable and push operators to land without one.
    let (_root, target, source) = repo_pair("abbrev", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");
    let short = git(&target, &["rev-parse", "--short", "HEAD"]).trim().to_owned();

    let mut plan = request(&source, &target);
    plan.expected_target_head = Some(short);
    assert!(matches!(
        LandQueue::new().land(&plan),
        LandOutcome::Landed { .. }
    ));
}

#[test]
fn expected_head_abbreviations_shorter_than_four_are_refused() {
    let (_root, target, source) = repo_pair("abbrev-too-short", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");
    let full = git(&target, &["rev-parse", "HEAD"]).trim().to_owned();

    for length in 1..=3 {
        let mut plan = request(&source, &target);
        plan.expected_target_head = Some(full[..length].to_owned());
        assert!(
            matches!(
                LandQueue::new().land(&plan),
                LandOutcome::Refused(LandRefusal::TargetMoved { .. })
            ),
            "expected a {length}-character pin to be refused"
        );
    }
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\n"
    );
}

#[test]
fn a_dirty_target_is_refused_before_anything_is_written() {
    let (_root, target, source) = repo_pair("dirty", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");
    std::fs::write(target.join("scratch.txt"), "uncommitted\n").expect("dirty file");

    match refusal(LandQueue::new().land(&request(&source, &target))) {
        LandRefusal::TargetDirty { paths } => {
            assert!(paths.iter().any(|path| path.contains("scratch.txt")), "{paths:?}");
        }
        other => panic!("expected TargetDirty, got {other}"),
    }
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\n"
    );
}

#[test]
fn a_session_with_no_changes_is_refused_rather_than_reported_as_landed() {
    let (_root, target, source) = repo_pair("empty", "one\n");
    assert_eq!(
        refusal(LandQueue::new().land(&request(&source, &target))),
        LandRefusal::NothingToLand
    );
}

#[test]
fn a_patch_that_no_longer_applies_is_refused_whole() {
    let (_root, target, source) = repo_pair("stale", "one\ntwo\nthree\n");
    std::fs::write(source.join("file.txt"), "one\nTWO\nthree\n").expect("edit");
    // The target's copy of that same line changed underneath.
    std::fs::write(target.join("file.txt"), "one\nsomething else\nthree\n").expect("target edit");
    git(&target, &["add", "."]);
    git(&target, &["commit", "--quiet", "-m", "target diverged"]);

    match refusal(LandQueue::new().land(&request(&source, &target))) {
        LandRefusal::PatchDidNotApply { detail } => assert!(!detail.is_empty()),
        other => panic!("expected PatchDidNotApply, got {other}"),
    }
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\nsomething else\nthree\n",
        "a refused patch must not have written any hunk"
    );
}

#[test]
fn a_multi_file_patch_that_partly_applies_is_refused_whole() {
    // The failure this exists to prevent: one file updated, the next rejected,
    // and a target left in a state no one reviewed. `git apply` checks every
    // hunk before writing any, and the `--check` pass makes that explicit.
    let (_root, target, source) = repo_pair("partial", "one\n");
    std::fs::write(source.join("file.txt"), "one\nappended\n").expect("edit one");
    std::fs::write(source.join("second.txt"), "brand new\n").expect("edit two");
    git(&source, &["add", "second.txt"]);

    // Make only the *second* file un-appliable, by creating it in the target
    // with different contents.
    std::fs::write(target.join("second.txt"), "already here\n").expect("target second");
    git(&target, &["add", "."]);
    git(&target, &["commit", "--quiet", "-m", "target has second.txt"]);

    let outcome = LandQueue::new().land(&request(&source, &target));
    assert!(
        matches!(outcome, LandOutcome::Refused(LandRefusal::PatchDidNotApply { .. })),
        "expected a whole refusal, got {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\n",
        "the file that WOULD have applied must be untouched"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("second.txt")).expect("second file"),
        "already here\n"
    );
}

#[test]
fn a_failing_verify_reverses_the_patch_and_leaves_the_target_unchanged() {
    let (_root, target, source) = repo_pair("verify-fail", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");
    let before = git(&target, &["status", "--porcelain"]);

    let mut plan = request(&source, &target);
    // `git --version` always succeeds; `git rev-parse --verify` on a missing ref
    // always fails, and needs no shell.
    plan.verify = vec![
        "git".to_owned(),
        "rev-parse".to_owned(),
        "--verify".to_owned(),
        "refs/heads/definitely-not-a-branch".to_owned(),
    ];

    match refusal(LandQueue::new().land(&plan)) {
        LandRefusal::VerifyFailed { command, code, .. } => {
            assert!(command.contains("definitely-not-a-branch"));
            assert_ne!(code, Some(0));
        }
        other => panic!("expected VerifyFailed, got {other}"),
    }
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\n",
        "a failed verify must leave no trace of the patch"
    );
    assert_eq!(git(&target, &["status", "--porcelain"]), before);
}

#[test]
fn a_passing_verify_keeps_the_change() {
    let (_root, target, source) = repo_pair("verify-pass", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");

    let mut plan = request(&source, &target);
    plan.verify = vec!["git".to_owned(), "--version".to_owned()];
    match LandQueue::new().land(&plan) {
        LandOutcome::Landed { verified, .. } => assert_eq!(verified, Some(true)),
        LandOutcome::Refused(refusal) => panic!("expected a landing, got {refusal}"),
    }
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\ntwo\n"
    );
}

#[test]
fn an_absurd_verify_timeout_is_clamped_rather_than_fatal() {
    let (_root, target, source) = repo_pair("verify-timeout-overflow", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");

    let mut plan = request(&source, &target);
    plan.verify = vec!["git".to_owned(), "--version".to_owned()];
    plan.verify_timeout_secs = Some(u64::MAX);

    match LandQueue::new().land(&plan) {
        LandOutcome::Landed { verified, .. } => assert_eq!(verified, Some(true)),
        LandOutcome::Refused(refusal) => panic!("expected a landing, got {refusal}"),
    }
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\ntwo\n"
    );
}

#[test]
#[cfg(windows)]
fn a_verify_command_that_hangs_is_refused_and_reversed() {
    let (_root, target, source) = repo_pair("verify-timeout", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");

    let mut plan = request(&source, &target);
    // Genuinely blocks for far longer than the deadline. A command reading
    // stdin would not do: the runner hands a verify command a null stdin, so it
    // sees EOF and exits at once.
    plan.verify = vec![
        "powershell".to_owned(),
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-Command".to_owned(),
        "Start-Sleep -Seconds 30".to_owned(),
    ];
    plan.verify_timeout_secs = Some(1);

    let outcome = LandQueue::new().land(&plan);
    assert!(
        matches!(outcome, LandOutcome::Refused(LandRefusal::VerifyFailed { .. })),
        "a hung verify must refuse, not hang the queue: {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\n"
    );
}

#[test]
fn a_target_outside_a_repository_is_refused_rather_than_assumed_safe() {
    let root = scratch("not-a-repo");
    let target = root.0.join("plain");
    std::fs::create_dir_all(&target).expect("plain dir");
    match refusal(LandQueue::new().land(&request(&target, &target))) {
        LandRefusal::Unavailable { detail } => assert!(!detail.is_empty()),
        other => panic!("expected Unavailable, got {other}"),
    }
}

#[test]
fn landings_are_serialised_so_the_second_sees_the_first() {
    // Without the gate both threads read the same clean target, both pass their
    // preconditions, and both apply — which is the incoherent-merge failure this
    // whole module exists to prevent.
    use std::sync::Arc;

    let (_root, target, source) = repo_pair("serialised", "one\n");
    std::fs::write(source.join("file.txt"), "one\ntwo\n").expect("edit");

    let queue = Arc::new(LandQueue::new());
    let plans: Vec<_> = (0..2).map(|_| request(&source, &target)).collect();
    let handles: Vec<_> = plans
        .into_iter()
        .map(|plan| {
            let queue = Arc::clone(&queue);
            std::thread::spawn(move || queue.land(&plan))
        })
        .collect();
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("land thread"))
        .collect();

    let landed = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, LandOutcome::Landed { .. }))
        .count();
    assert_eq!(
        landed, 1,
        "exactly one landing may succeed; the second must see a dirty target: {outcomes:?}"
    );
    assert!(outcomes.iter().any(|outcome| matches!(
        outcome,
        LandOutcome::Refused(LandRefusal::TargetDirty { .. })
    )));
    // And the file reflects one landing, not two applications of the same hunk.
    assert_eq!(
        std::fs::read_to_string(target.join("file.txt")).expect("target file"),
        "one\ntwo\n"
    );
}
