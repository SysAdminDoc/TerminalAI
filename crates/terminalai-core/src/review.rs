//! Read-only aggregation of pending Git work for the review surface.
//!
//! The daemon owns this query so every client sees the same working-tree
//! snapshot. It never stages, commits, resolves, or otherwise mutates a
//! repository; conflict markers are returned as data for the operator.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::io::Read;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::process_tree::ProcessJob;
use crate::{Agent, Session, SessionId};

pub const MAX_REVIEW_DIFF_BYTES: usize = 128 * 1024;
pub const REVIEW_REPOSITORY_TIMEOUT: Duration = Duration::from_secs(5);
/// Branch lookup runs on the hook path, so it gets a much tighter budget than a
/// review collection: a slow repository must never stall status ingestion.
pub const BRANCH_TIMEOUT: Duration = Duration::from_millis(1500);
const REVIEW_WORKER_COUNT: usize = 4;
const REVIEW_COMMAND_OUTPUT_BYTES: usize = MAX_REVIEW_DIFF_BYTES;
const REVIEW_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewItem {
    pub session_id: SessionId,
    pub name: String,
    pub agent: Agent,
    pub cwd: PathBuf,
    pub files_changed: usize,
    pub additions: u64,
    pub deletions: u64,
    /// A weighted ordering score: conflicts first, then changed lines/files.
    pub review_cost: u64,
    pub conflicts: Vec<String>,
    pub conflict_markers: u32,
    /// True only while the operator's mark still matches `state_digest`.
    pub reviewed: bool,
    /// Fingerprint of the diff this item describes. The reviewed mark is stored
    /// against this value, so any change to the tree retires the mark.
    #[serde(default)]
    pub state_digest: String,
    /// The commit this review was read against. The land gate refuses when the
    /// repository has moved past it, so without this the moved-target check
    /// would have nothing to compare and could never fire.
    #[serde(default)]
    pub target_head: Option<String>,
    pub diff: String,
    pub diff_truncated: bool,
    #[serde(default)]
    pub timed_out: bool,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum ReviewError {
    #[error("git command could not start: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {command} failed: {message}")]
    Command { command: String, message: String },
    #[error("git process could not be contained: {0}")]
    Containment(String),
    #[error("git command wait failed: {0}")]
    Wait(String),
}

#[derive(Debug, Default)]
struct DiffStats {
    paths: BTreeSet<String>,
    additions: u64,
    deletions: u64,
}

#[derive(Debug, Default)]
struct ReviewData {
    stats: DiffStats,
    conflicts: Vec<String>,
    conflict_markers: u32,
    diff: String,
    diff_truncated: bool,
    timed_out: bool,
    timed_out_command: Option<String>,
}

#[derive(Debug, Default)]
struct CappedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug, Default)]
struct GitOutput {
    stdout: CappedOutput,
    stderr: CappedOutput,
}

enum GitOutcome {
    Completed(GitOutput),
    TimedOut(GitOutput),
}

/// Collect one session's current working-tree diff, including staged changes.
pub fn collect_review(session: &Session) -> ReviewItem {
    let mut item = ReviewItem {
        session_id: session.id.clone(),
        name: session.name.clone(),
        agent: session.agent,
        cwd: session.cwd.clone(),
        files_changed: 0,
        additions: 0,
        deletions: 0,
        review_cost: 0,
        conflicts: Vec::new(),
        conflict_markers: 0,
        reviewed: false,
        state_digest: String::new(),
        target_head: None,
        diff: String::new(),
        diff_truncated: false,
        timed_out: false,
        error: None,
    };

    item.target_head = head_commit(&session.cwd);
    let result = collect_git_review(&session.cwd, REVIEW_REPOSITORY_TIMEOUT);
    match result {
        Ok(data) => {
            item.files_changed = data.stats.paths.len().max(data.conflicts.len());
            item.additions = data.stats.additions;
            item.deletions = data.stats.deletions;
            item.conflicts = data.conflicts;
            item.conflict_markers = data.conflict_markers;
            item.review_cost = (item.files_changed as u64 * 10)
                .saturating_add(item.additions)
                .saturating_add(item.deletions)
                .saturating_add(u64::from(item.conflict_markers) * 1_000);
            item.diff = data.diff;
            item.diff_truncated = data.diff_truncated;
            item.timed_out = data.timed_out;
            if data.timed_out {
                let command = data.timed_out_command.unwrap_or_else(|| "git".into());
                item.error = Some(format!(
                    "Review timed out after {} seconds while running git {}; partial results shown",
                    REVIEW_REPOSITORY_TIMEOUT.as_secs(),
                    command
                ));
            }
        }
        Err(error) => item.error = Some(error.to_string()),
    }
    item.state_digest = review_state_digest(&item);
    // A mark only survives while it still describes what is on disk.
    item.reviewed = session
        .reviewed_digest
        .as_deref()
        .is_some_and(|marked| marked == item.state_digest);
    item
}

/// Fingerprint the reviewed state of a working tree.
///
/// Built from the numstat totals plus every changed and conflicted path, which
/// `collect_review` already has — so this costs no extra Git process. A digest
/// that cannot be computed (an errored or timed-out collection) is empty, and an
/// empty digest never matches a stored mark, so an unreadable repository degrades
/// to unreviewed rather than to a stale acknowledgement.
fn review_state_digest(item: &ReviewItem) -> String {
    if item.error.is_some() || item.timed_out {
        return String::new();
    }
    let mut hasher = DefaultHasher::new();
    item.files_changed.hash(&mut hasher);
    item.additions.hash(&mut hasher);
    item.deletions.hash(&mut hasher);
    item.conflict_markers.hash(&mut hasher);
    for conflict in &item.conflicts {
        conflict.hash(&mut hasher);
    }
    // The diff text itself catches edits that leave the line counts unchanged —
    // a rename, a reordering, or one line swapped for another.
    item.diff.hash(&mut hasher);
    item.diff_truncated.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Collect many repositories with a fixed number of workers so one slow Git
/// process cannot turn every session into another simultaneous subprocess.
pub(crate) fn collect_reviews(sessions: Vec<Session>) -> Vec<ReviewItem> {
    if sessions.is_empty() {
        return Vec::new();
    }

    let worker_count = sessions.len().min(REVIEW_WORKER_COUNT);
    let (job_sender, job_receiver) = mpsc::sync_channel(sessions.len());
    let job_receiver = Arc::new(Mutex::new(job_receiver));
    let (result_sender, result_receiver) = mpsc::channel();
    let mut workers = Vec::with_capacity(worker_count);

    for index in 0..worker_count {
        let job_receiver = Arc::clone(&job_receiver);
        let result_sender = result_sender.clone();
        // `thread::Builder::spawn` where the workspace uses it everywhere else:
        // `std::thread::spawn` panics on the one condition this pool can
        // actually hit — thread exhaustion on a fleet whose every session
        // already owns a reader, a writer, a monitor and a timer.
        let worker = thread::Builder::new()
            .name(format!("terminalai-review-{index}"))
            .spawn(move || loop {
                let session = {
                    let receiver = job_receiver.lock().expect("review worker queue lock");
                    receiver.recv().ok()
                };
                let Some(session) = session else {
                    break;
                };
                let item = collect_review(&session);
                if result_sender.send(item).is_err() {
                    break;
                }
            });
        match worker {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                tracing::warn!(%error, "could not start a review worker; continuing with fewer");
                break;
            }
        }
    }
    drop(result_sender);
    if workers.is_empty() {
        // Nothing will drain the queue, and the collector below would block on a
        // channel no worker is feeding. Say so rather than hang the caller.
        tracing::error!("no review workers could be started; reporting no reviews");
        return Vec::new();
    }

    for session in sessions {
        if job_sender.send(session).is_err() {
            break;
        }
    }
    drop(job_sender);

    let reviews: Vec<_> = result_receiver
        .into_iter()
        .filter(|item| item.files_changed > 0 || item.error.is_some())
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
    reviews
}

fn collect_git_review(cwd: &Path, timeout: Duration) -> Result<ReviewData, ReviewError> {
    let deadline = Instant::now() + timeout;
    let mut data = ReviewData::default();
    let numstat_args = ["diff", "HEAD", "--no-renames", "--numstat", "--"];
    let numstat = git(cwd, &numstat_args, deadline)?;
    match numstat {
        GitOutcome::Completed(output) => {
            data.stats = parse_numstat(&String::from_utf8_lossy(&output.stdout.bytes));
        }
        GitOutcome::TimedOut(output) => {
            data.stats = parse_numstat(&String::from_utf8_lossy(&output.stdout.bytes));
            mark_timed_out(&mut data, &numstat_args);
            return Ok(data);
        }
    }

    let diff_args = ["diff", "HEAD", "--no-ext-diff", "--unified=3", "--"];
    let diff = git(cwd, &diff_args, deadline)?;
    match diff {
        GitOutcome::Completed(output) => {
            data.diff_truncated = output.stdout.truncated;
            data.diff = bounded_diff(output.stdout);
            data.conflict_markers = count_conflict_markers(&data.diff);
        }
        GitOutcome::TimedOut(output) => {
            data.diff_truncated = output.stdout.truncated;
            data.diff = bounded_diff(output.stdout);
            data.conflict_markers = count_conflict_markers(&data.diff);
            mark_timed_out(&mut data, &diff_args);
            return Ok(data);
        }
    }

    let status_args = ["status", "--porcelain=v1", "--untracked-files=no"];
    let status = git(cwd, &status_args, deadline)?;
    match status {
        GitOutcome::Completed(output) => {
            data.conflicts = parse_conflicts(&String::from_utf8_lossy(&output.stdout.bytes));
        }
        GitOutcome::TimedOut(output) => {
            data.conflicts = parse_conflicts(&String::from_utf8_lossy(&output.stdout.bytes));
            mark_timed_out(&mut data, &status_args);
            return Ok(data);
        }
    }
    Ok(data)
}

fn mark_timed_out(data: &mut ReviewData, args: &[&str]) {
    data.timed_out = true;
    data.timed_out_command = Some(args.join(" "));
}

fn git(cwd: &Path, args: &[&str], deadline: Instant) -> Result<GitOutcome, ReviewError> {
    run_git_program("git", cwd, args, deadline)
}

/// The checked-out branch for `cwd`, or `None` when there is nothing honest to
/// show.
///
/// Returns `None` — never a guess — for a directory outside a repository, a
/// detached HEAD, an unborn branch, or a Git that does not answer inside
/// [`BRANCH_TIMEOUT`]. The fleet row renders an em dash in those cases, which is
/// the truth; inventing "main" would not be.
/// The commit `cwd` is currently on, or `None` when it cannot be read.
///
/// Shares the branch lookup's tight budget: this runs once per reviewed session
/// and a slow repository must not stall the whole review collection.
pub fn head_commit(cwd: &Path) -> Option<String> {
    let deadline = Instant::now() + BRANCH_TIMEOUT;
    let GitOutcome::Completed(output) = git(cwd, &["rev-parse", "HEAD"], deadline).ok()? else {
        return None;
    };
    let head = String::from_utf8_lossy(&output.stdout.bytes).trim().to_owned();
    (!head.is_empty()).then_some(head)
}

pub fn current_branch(cwd: &Path) -> Option<String> {
    let deadline = Instant::now() + BRANCH_TIMEOUT;
    let GitOutcome::Completed(output) =
        git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"], deadline).ok()?
    else {
        return None;
    };
    let branch = String::from_utf8_lossy(&output.stdout.bytes)
        .trim()
        .to_owned();
    if branch.is_empty() || branch == "HEAD" {
        return None;
    }
    Some(branch)
}

fn run_git_program(
    program: &str,
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
) -> Result<GitOutcome, ReviewError> {
    let command = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    if Instant::now() >= deadline {
        return Ok(GitOutcome::TimedOut(GitOutput::default()));
    }

    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    #[cfg(windows)]
    let job = match ProcessJob::assign(child.as_raw_handle()) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ReviewError::Containment(error));
        }
    };

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            #[cfg(windows)]
            terminate_child(&mut child, &job);
            #[cfg(not(windows))]
            terminate_child(&mut child);
            return Err(ReviewError::Spawn(std::io::Error::other(
                "git stdout pipe missing",
            )));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            #[cfg(windows)]
            terminate_child(&mut child, &job);
            #[cfg(not(windows))]
            terminate_child(&mut child);
            return Err(ReviewError::Spawn(std::io::Error::other(
                "git stderr pipe missing",
            )));
        }
    };
    // A capture thread that cannot start would leave the child writing into a
    // pipe nobody drains, which deadlocks once it fills. Refuse the run instead.
    let stdout_reader = match spawn_capture("stdout", stdout) {
        Ok(reader) => reader,
        Err(error) => {
            #[cfg(windows)]
            terminate_child(&mut child, &job);
            #[cfg(not(windows))]
            terminate_child(&mut child);
            return Err(ReviewError::Spawn(error));
        }
    };
    let stderr_reader = match spawn_capture("stderr", stderr) {
        Ok(reader) => reader,
        Err(error) => {
            #[cfg(windows)]
            terminate_child(&mut child, &job);
            #[cfg(not(windows))]
            terminate_child(&mut child);
            let _ = join_capture(stdout_reader);
            return Err(ReviewError::Spawn(error));
        }
    };

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    #[cfg(windows)]
                    terminate_child(&mut child, &job);
                    #[cfg(not(windows))]
                    terminate_child(&mut child);
                    #[cfg(windows)]
                    drop(job);
                    let output = GitOutput {
                        stdout: join_capture(stdout_reader),
                        stderr: join_capture(stderr_reader),
                    };
                    return Ok(GitOutcome::TimedOut(output));
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(remaining.min(REVIEW_POLL_INTERVAL));
            }
            Err(error) => {
                #[cfg(windows)]
                terminate_child(&mut child, &job);
                #[cfg(not(windows))]
                terminate_child(&mut child);
                #[cfg(windows)]
                drop(job);
                let _ = join_capture(stdout_reader);
                let _ = join_capture(stderr_reader);
                return Err(ReviewError::Wait(error.to_string()));
            }
        }
    };

    #[cfg(windows)]
    drop(job);
    let output = GitOutput {
        stdout: join_capture(stdout_reader),
        stderr: join_capture(stderr_reader),
    };
    if !status.success() {
        let message = String::from_utf8_lossy(&output.stderr.bytes)
            .trim()
            .to_string();
        return Err(ReviewError::Command {
            command,
            message: if message.is_empty() {
                format!("exit status {status}")
            } else {
                message
            },
        });
    }
    Ok(GitOutcome::Completed(output))
}

fn spawn_capture<R>(stream: &str, mut reader: R) -> std::io::Result<JoinHandle<CappedOutput>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("terminalai-review-capture-{stream}"))
        .spawn(move || {
        let mut output = CappedOutput {
            bytes: Vec::with_capacity(REVIEW_COMMAND_OUTPUT_BYTES.min(8 * 1024)),
            truncated: false,
        };
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let remaining = REVIEW_COMMAND_OUTPUT_BYTES.saturating_sub(output.bytes.len());
                    let retained = read.min(remaining);
                    output.bytes.extend_from_slice(&buffer[..retained]);
                    output.truncated |= retained < read;
                }
                Err(_) => break,
            }
        }
        output
    })
}

fn join_capture(reader: JoinHandle<CappedOutput>) -> CappedOutput {
    reader.join().unwrap_or_default()
}

#[cfg(windows)]
fn terminate_child(child: &mut Child, job: &ProcessJob) {
    let _ = job.terminate();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(windows))]
fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn parse_numstat(text: &str) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in text.lines() {
        let mut fields = line.splitn(3, '\t');
        let Some(additions) = fields.next() else {
            continue;
        };
        let Some(deletions) = fields.next() else {
            continue;
        };
        let Some(path) = fields.next() else {
            continue;
        };
        stats.paths.insert(path.to_string());
        stats.additions = stats
            .additions
            .saturating_add(additions.parse::<u64>().unwrap_or(0));
        stats.deletions = stats
            .deletions
            .saturating_add(deletions.parse::<u64>().unwrap_or(0));
    }
    stats
}

fn parse_conflicts(text: &str) -> Vec<String> {
    let mut conflicts = BTreeSet::new();
    for line in text.lines() {
        let code = line.get(..2).unwrap_or_default();
        if matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
            if let Some(path) = line.get(3..) {
                conflicts.insert(path.to_string());
            }
        }
    }
    conflicts.into_iter().collect()
}

fn count_conflict_markers(diff: &str) -> u32 {
    let mut stage = 0_u8;
    let mut complete_blocks = 0_u32;
    for line in diff.lines() {
        if line.starts_with("+<<<<<<<") {
            stage = 1;
        } else if stage == 1 && line.starts_with("+=======") {
            stage = 2;
        } else if stage == 2 && line.starts_with("+>>>>>>>") {
            complete_blocks = complete_blocks.saturating_add(1);
            stage = 0;
        }
    }
    complete_blocks.saturating_mul(3)
}

fn bounded_diff(output: CappedOutput) -> String {
    let mut diff = String::from_utf8_lossy(&output.bytes).into_owned();
    if output.truncated {
        diff.push_str("\n\n[diff truncated at 128 KiB]\n");
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_counts_files_and_lines_without_shell_parsing() {
        let stats = parse_numstat("12\t3\tsrc/main.rs\n-\t-\tassets/logo.png\n");
        assert_eq!(stats.paths.len(), 2);
        assert_eq!(stats.additions, 12);
        assert_eq!(stats.deletions, 3);
    }

    #[test]
    fn conflict_statuses_and_added_markers_are_preserved() {
        let conflicts = parse_conflicts("UU\tsrc/main.rs\n M\tREADME.md\n");
        assert_eq!(conflicts, ["src/main.rs"]);
        assert_eq!(
            count_conflict_markers("+<<<<<<< HEAD\n+=======\n+>>>>>>> theirs\n"),
            3
        );
        assert_eq!(count_conflict_markers("+=======\n"), 0);
        assert_eq!(count_conflict_markers("+<<<<<<< HEAD\n+=======\n"), 0);
    }

    #[test]
    fn large_diffs_are_bounded_for_the_control_plane() {
        let diff = bounded_diff(CappedOutput {
            bytes: vec![b'x'; MAX_REVIEW_DIFF_BYTES],
            truncated: true,
        });
        assert!(diff.len() < MAX_REVIEW_DIFF_BYTES + 64);
        assert!(diff.contains("diff truncated"));
    }

    #[test]
    fn command_capture_caps_output_while_draining_the_pipe() {
        let reader = std::io::Cursor::new(vec![b'x'; MAX_REVIEW_DIFF_BYTES + 1]);
        let captured = join_capture(spawn_capture("test", reader).expect("capture thread"));
        assert_eq!(captured.bytes.len(), MAX_REVIEW_DIFF_BYTES);
        assert!(captured.truncated);
    }

    #[test]
    fn git_command_timeout_returns_a_bounded_partial_result() {
        let root = std::env::temp_dir().join(format!(
            "terminalai-review-timeout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create timeout directory");

        #[cfg(windows)]
        let (program, args) = ("cmd", vec!["/C", "ping", "127.0.0.1", "-n", "10"]);
        #[cfg(not(windows))]
        let (program, args) = ("sh", vec!["-c", "while true; do :; done"]);
        let outcome = run_git_program(
            program,
            &root,
            &args,
            Instant::now() + Duration::from_millis(100),
        )
        .expect("start timeout command");
        assert!(matches!(outcome, GitOutcome::TimedOut(_)));
        std::fs::remove_dir_all(root).expect("remove timeout directory");
    }

    #[test]
    fn collects_worktree_changes_without_mutating_the_repository() {
        let root = std::env::temp_dir().join(format!(
            "terminalai-review-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create temporary repository");
        let run_git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("start git");
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "terminalai-tests@example.invalid"]);
        run_git(&["config", "user.name", "TerminalAI tests"]);
        let file = root.join("README.txt");
        std::fs::write(&file, "before\n").expect("write baseline");
        run_git(&["add", "README.txt"]);
        run_git(&["commit", "-qm", "baseline"]);
        std::fs::write(&file, "after\n").expect("write change");

        let spec = crate::launch::spec_for(crate::Agent::Claude, &root);
        let item = collect_review(&Session::new(SessionId::new(1), &spec));
        assert_eq!(item.files_changed, 1);
        assert_eq!(item.additions, 1);
        assert_eq!(item.deletions, 1);
        assert!(!item.diff_truncated);
        assert!(item.diff.contains("-before"));
        assert!(item.diff.contains("+after"));
        assert!(root.join(".git").is_dir());
        std::fs::remove_dir_all(root).expect("remove temporary repository");
    }

    #[test]
    fn the_branch_is_read_from_head_and_absent_rather_than_guessed() {
        let root = std::env::temp_dir().join(format!(
            "terminalai-branch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create temporary directory");

        // Outside a repository there is no branch, and none is invented.
        assert_eq!(current_branch(&root), None);

        let run_git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("start git");
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "-q", "-b", "trunk"]);
        run_git(&["config", "user.email", "terminalai-tests@example.invalid"]);
        run_git(&["config", "user.name", "TerminalAI tests"]);
        std::fs::write(root.join("README.txt"), "x\n").expect("write");
        run_git(&["add", "README.txt"]);
        run_git(&["commit", "-qm", "baseline"]);

        assert_eq!(current_branch(&root).as_deref(), Some("trunk"));

        run_git(&["checkout", "-q", "-b", "feature/rewrite"]);
        assert_eq!(current_branch(&root).as_deref(), Some("feature/rewrite"));

        // A detached HEAD has no branch name; reporting the previous one would lie.
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .expect("read head");
        let sha = String::from_utf8_lossy(&head.stdout).trim().to_owned();
        run_git(&["checkout", "-q", &sha]);
        assert_eq!(current_branch(&root), None);

        std::fs::remove_dir_all(root).expect("remove temporary repository");
    }

    #[test]
    fn a_reviewed_mark_expires_when_the_agent_changes_another_file() {
        let root = std::env::temp_dir().join(format!(
            "terminalai-review-expiry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create temporary repository");
        let run_git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("start git");
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "terminalai-tests@example.invalid"]);
        run_git(&["config", "user.name", "TerminalAI tests"]);
        std::fs::write(root.join("README.txt"), "before\n").expect("write baseline");
        run_git(&["add", "README.txt"]);
        run_git(&["commit", "-qm", "baseline"]);
        std::fs::write(root.join("README.txt"), "after\n").expect("write change");

        let spec = crate::launch::spec_for(crate::Agent::Claude, &root);
        let mut session = Session::new(SessionId::new(1), &spec);

        let before = collect_review(&session);
        assert!(!before.reviewed, "a fresh session starts unreviewed");
        assert!(!before.state_digest.is_empty());

        // The operator reviews what they can see.
        session.reviewed_digest = Some(before.state_digest.clone());
        let marked = collect_review(&session);
        assert!(marked.reviewed, "the mark holds while nothing has changed");
        assert_eq!(marked.state_digest, before.state_digest);

        // The agent keeps working.
        std::fs::write(root.join("NOTES.txt"), "agent kept going\n").expect("write new file");
        run_git(&["add", "NOTES.txt"]);
        let after = collect_review(&session);
        assert_ne!(
            after.state_digest, before.state_digest,
            "a new file must change the reviewed state digest"
        );
        assert!(
            !after.reviewed,
            "the row must return to unreviewed once the diff moves"
        );

        // An edit that leaves the line counts identical must still retire the mark.
        session.reviewed_digest = Some(after.state_digest.clone());
        assert!(collect_review(&session).reviewed);
        std::fs::write(root.join("NOTES.txt"), "agent changed its mind\n").expect("rewrite");
        let swapped = collect_review(&session);
        assert_eq!(swapped.additions, after.additions, "same line counts");
        assert!(
            !swapped.reviewed,
            "a same-size edit must still retire the mark"
        );

        std::fs::remove_dir_all(root).expect("remove temporary repository");
    }
}
