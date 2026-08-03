//! Landing a session's work into a target repository, or refusing to.
//!
//! Every tool in the survey stops at "open a PR", which leaves the operator
//! serialising and testing each landing by hand. The failure that costs most is
//! not a textual conflict — Git catches those — but two agents that each looked
//! correct alone and are incoherent together. So this module does two things and
//! no more: it serialises landings, and it refuses loudly.
//!
//! Three rules hold everywhere here:
//!
//! 1. **Never partial.** A landing either applies whole or leaves the target
//!    exactly as it was. A verify failure reverses the patch it just applied.
//! 2. **Never auto-resolved.** Nothing merges, rebases, stages, commits, or
//!    picks a side on the operator's behalf. A conflict is a refusal.
//! 3. **Re-read at land time.** The queue may have held this request while other
//!    landings changed the target, so every precondition is checked against a
//!    fresh read, never against what the review surface showed.

use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::process_tree::ProcessJob;

/// How long any single Git step may take before the landing is refused.
pub const LAND_GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default budget for the operator's verify command. Overridable per request,
/// because a real test suite is routinely slower than any Git operation.
pub const DEFAULT_VERIFY_TIMEOUT: Duration = Duration::from_secs(600);

/// What the operator asked to land, and what it must be true against.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LandRequest {
    /// The session's working tree, whose uncommitted diff is the payload.
    pub source: PathBuf,
    /// The repository the change lands in.
    pub target: PathBuf,
    /// The target commit the operator reviewed against. A landing is refused
    /// when the target has moved since — that is the whole point of re-reading.
    /// `None` means the operator did not pin one, and only the dirty/conflict
    /// checks apply.
    #[serde(default)]
    pub expected_target_head: Option<String>,
    /// Command and arguments run in the target after applying. Empty means the
    /// operator configured no verification, which is recorded rather than
    /// treated as a pass.
    #[serde(default)]
    pub verify: Vec<String>,
    #[serde(default)]
    pub verify_timeout_secs: Option<u64>,
}

/// Why a landing did not happen. Every variant names one specific condition;
/// there is deliberately no catch-all "could not land".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum LandRefusal {
    /// The target advanced while this request sat in the queue, or between
    /// review and landing.
    TargetMoved { expected: String, found: String },
    /// The target has uncommitted changes. Landing onto them would produce a
    /// tree no one reviewed, and would make the reversal path ambiguous.
    TargetDirty { paths: Vec<String> },
    /// Conflict markers are already present in one of the trees.
    ConflictMarkers { paths: Vec<String> },
    /// The patch no longer applies cleanly. Nothing was written.
    PatchDidNotApply { detail: String },
    /// The verify command failed. The patch was reversed.
    VerifyFailed {
        command: String,
        code: Option<i32>,
        output: String,
    },
    /// The verify command failed *and* the reversal also failed, so the target
    /// is in a mixed state. Loudest possible outcome: it names both failures
    /// and the patch needed to finish the reversal by hand.
    VerifyFailedAndNotReversed {
        command: String,
        reversal_error: String,
    },
    /// The session has no uncommitted work.
    NothingToLand,
    /// A precondition could not be read at all — no Git, not a repository, a
    /// step that timed out. Refusing is the only honest answer: an unreadable
    /// target cannot be shown to be safe.
    Unavailable { detail: String },
}

impl std::fmt::Display for LandRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LandRefusal::TargetMoved { expected, found } => write!(
                f,
                "target moved since review: expected {expected}, found {found}"
            ),
            LandRefusal::TargetDirty { paths } => {
                write!(f, "target has uncommitted changes: {}", paths.join(", "))
            }
            LandRefusal::ConflictMarkers { paths } => {
                write!(f, "conflict markers present in: {}", paths.join(", "))
            }
            LandRefusal::PatchDidNotApply { detail } => {
                write!(f, "patch no longer applies: {detail}")
            }
            LandRefusal::VerifyFailed {
                command,
                code,
                output,
            } => write!(
                f,
                "verify command {command:?} failed{}; the patch was reversed and the target is unchanged: {output}",
                code.map(|code| format!(" with exit code {code}")).unwrap_or_default()
            ),
            LandRefusal::VerifyFailedAndNotReversed {
                command,
                reversal_error,
            } => write!(
                f,
                "verify command {command:?} failed AND the patch could not be reversed ({reversal_error}); \
                 the target is in a mixed state and needs manual repair"
            ),
            LandRefusal::NothingToLand => write!(f, "the session has no uncommitted changes"),
            LandRefusal::Unavailable { detail } => {
                write!(f, "landing preconditions could not be read: {detail}")
            }
        }
    }
}

/// The result of one landing attempt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum LandOutcome {
    /// The patch applied and, if one was configured, the verify command passed.
    /// The change is left in the target's working tree, unstaged and
    /// uncommitted — committing is the operator's decision, not this module's.
    Landed {
        files_changed: usize,
        /// The target commit the patch was applied on top of.
        target_head: String,
        /// `None` when no verify command was configured. Recorded as absent
        /// rather than reported as a pass.
        verified: Option<bool>,
    },
    Refused(LandRefusal),
}

/// Serialises every landing in the process.
///
/// Two agents landing at once is precisely the case the community works around
/// with hand-built merge queues: each landing must see the result of the last,
/// which is impossible if their precondition checks interleave. A single mutex
/// held for the whole attempt is the entire mechanism.
#[derive(Debug, Default)]
pub struct LandQueue {
    gate: Mutex<()>,
}

impl LandQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Land one request, or refuse it. Blocks until any in-flight landing ends.
    pub fn land(&self, request: &LandRequest) -> LandOutcome {
        // A poisoned gate means a previous landing panicked mid-flight. The
        // lock is still usable and the next attempt re-reads every precondition
        // from disk anyway, so recovering is safe and refusing here would strand
        // the queue for the life of the daemon.
        let _serialised = self.gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        land_now(request)
    }
}

/// One landing attempt, already serialised.
fn land_now(request: &LandRequest) -> LandOutcome {
    match run_land(request) {
        Ok(outcome) => outcome,
        Err(refusal) => LandOutcome::Refused(refusal),
    }
}

fn run_land(request: &LandRequest) -> Result<LandOutcome, LandRefusal> {
    // Every check below reads the disk now, not what the review surface showed.
    let target_head = read_head(&request.target)?;
    if let Some(expected) = &request.expected_target_head {
        if !head_matches(expected, &target_head) {
            return Err(LandRefusal::TargetMoved {
                expected: expected.clone(),
                found: target_head,
            });
        }
    }

    let dirty = dirty_paths(&request.target)?;
    if !dirty.is_empty() {
        return Err(LandRefusal::TargetDirty { paths: dirty });
    }

    let conflicted = conflicted_paths(&request.source)?;
    if !conflicted.is_empty() {
        return Err(LandRefusal::ConflictMarkers { paths: conflicted });
    }

    let patch = source_patch(&request.source)?;
    if patch.trim().is_empty() {
        return Err(LandRefusal::NothingToLand);
    }

    // `git apply` checks every hunk before writing any of them, so a patch that
    // no longer fits is rejected whole. `--check` first keeps even a partially
    // written file out of the target when the failure is detectable up front.
    apply_patch(&request.target, &patch, ApplyMode::Check)?;
    apply_patch(&request.target, &patch, ApplyMode::Write)?;

    let files_changed = patch_file_count(&patch);
    if request.verify.is_empty() {
        return Ok(LandOutcome::Landed {
            files_changed,
            target_head,
            verified: None,
        });
    }

    let timeout = request
        .verify_timeout_secs
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_VERIFY_TIMEOUT);
    match run_verify(&request.target, &request.verify, timeout) {
        VerifyResult::Passed => Ok(LandOutcome::Landed {
            files_changed,
            target_head,
            verified: Some(true),
        }),
        VerifyResult::Failed { code, output } => {
            let command = request.verify.join(" ");
            // The landing must be whole or absent. Reverse exactly what was
            // applied — the target was clean beforehand, so this restores it.
            match apply_patch(&request.target, &patch, ApplyMode::Reverse) {
                Ok(()) => Err(LandRefusal::VerifyFailed {
                    command,
                    code,
                    output,
                }),
                Err(refusal) => Err(LandRefusal::VerifyFailedAndNotReversed {
                    command,
                    reversal_error: refusal.to_string(),
                }),
            }
        }
    }
}

fn head_matches(expected: &str, found: &str) -> bool {
    // An abbreviated hash from the review surface must still match the full one.
    let expected = expected.trim();
    let found = found.trim();
    !expected.is_empty() && (found.starts_with(expected) || expected.starts_with(found))
}

fn read_head(target: &Path) -> Result<String, LandRefusal> {
    let head = git_text(target, &["rev-parse", "HEAD"])?;
    let head = head.trim().to_owned();
    if head.is_empty() {
        return Err(LandRefusal::Unavailable {
            detail: format!("{} has no HEAD commit", target.display()),
        });
    }
    Ok(head)
}

/// Paths with uncommitted changes in `repo`, including untracked files.
fn dirty_paths(repo: &Path) -> Result<Vec<String>, LandRefusal> {
    let status = git_text(repo, &["status", "--porcelain=v1", "--untracked-files=normal"])?;
    Ok(status
        .lines()
        .filter_map(|line| {
            let path = line.get(3..)?.trim();
            (!path.is_empty()).then(|| path.to_owned())
        })
        .collect())
}

/// Paths in `repo` that Git reports as unmerged.
fn conflicted_paths(repo: &Path) -> Result<Vec<String>, LandRefusal> {
    let unmerged = git_text(repo, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(unmerged
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The session's uncommitted work as a patch.
fn source_patch(source: &Path) -> Result<String, LandRefusal> {
    git_text(
        source,
        &["diff", "HEAD", "--binary", "--no-ext-diff", "--no-renames"],
    )
}

#[derive(Clone, Copy)]
enum ApplyMode {
    Check,
    Write,
    Reverse,
}

fn apply_patch(target: &Path, patch: &str, mode: ApplyMode) -> Result<(), LandRefusal> {
    let mut args = vec!["apply", "--whitespace=nowarn"];
    match mode {
        ApplyMode::Check => args.push("--check"),
        ApplyMode::Write => {}
        ApplyMode::Reverse => args.push("--reverse"),
    }
    let run = run_program("git", target, &args, Some(patch), LAND_GIT_TIMEOUT);
    match run {
        ProcessRun::Completed {
            status: Some(0), ..
        } => Ok(()),
        ProcessRun::Completed { stderr, .. } => Err(LandRefusal::PatchDidNotApply {
            detail: first_meaningful_line(&stderr),
        }),
        ProcessRun::TimedOut => Err(LandRefusal::Unavailable {
            detail: format!("git apply did not finish within {LAND_GIT_TIMEOUT:?}"),
        }),
        ProcessRun::Failed { detail } => Err(LandRefusal::Unavailable { detail }),
    }
}

enum VerifyResult {
    Passed,
    Failed { code: Option<i32>, output: String },
}

fn run_verify(target: &Path, command: &[String], timeout: Duration) -> VerifyResult {
    let Some((program, args)) = command.split_first() else {
        return VerifyResult::Passed;
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_program(program, target, &args, None, timeout) {
        ProcessRun::Completed { status: Some(0), .. } => VerifyResult::Passed,
        ProcessRun::Completed {
            status,
            stdout,
            stderr,
        } => VerifyResult::Failed {
            code: status,
            output: verify_tail(&stdout, &stderr),
        },
        ProcessRun::TimedOut => VerifyResult::Failed {
            code: None,
            output: format!("verify command did not finish within {timeout:?}"),
        },
        ProcessRun::Failed { detail } => VerifyResult::Failed {
            code: None,
            output: detail,
        },
    }
}

/// The last few lines of a failed verify run. A test suite prints far more than
/// belongs in a refusal message, and the failure is at the end.
fn verify_tail(stdout: &str, stderr: &str) -> String {
    const KEEP_LINES: usize = 20;
    let combined = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let lines: Vec<&str> = combined.lines().filter(|line| !line.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(KEEP_LINES);
    lines[start..].join("\n")
}

fn first_meaningful_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("git apply reported no detail")
        .to_owned()
}

/// Count the files a patch touches, from its `diff --git` headers.
fn patch_file_count(patch: &str) -> usize {
    patch
        .lines()
        .filter(|line| line.starts_with("diff --git "))
        .count()
}

/// How a child process finished.
///
/// `review.rs` has a similar runner, but it collapses any nonzero exit into an
/// error and discards stdout. A refusal has to quote the verify command's exit
/// code and output, and `git apply` needs a patch on stdin, so this one keeps a
/// failing exit as data rather than as an error.
pub(crate) enum ProcessRun {
    Completed {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    TimedOut,
    Failed {
        detail: String,
    },
}

const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Enough to hold a failing test suite's tail without letting a runaway command
/// grow the daemon without bound.
const MAX_CAPTURED_BYTES: usize = 256 * 1024;

pub(crate) fn run_program(
    program: &str,
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> ProcessRun {
    let deadline = Instant::now() + timeout;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProcessRun::Failed {
                detail: format!("could not run {program}: {error}"),
            }
        }
    };

    // A verify command is operator-supplied and may spawn a whole toolchain;
    // containment is what stops an abandoned one outliving the daemon.
    #[cfg(windows)]
    let job = match ProcessJob::assign(child.as_raw_handle()) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return ProcessRun::Failed {
                detail: format!("{program} could not be contained: {error}"),
            };
        }
    };

    if let Some(input) = stdin {
        // Written on a thread: a patch larger than the pipe buffer would
        // otherwise deadlock against a child that is not reading yet.
        if let Some(mut pipe) = child.stdin.take() {
            let input = input.to_owned();
            thread::spawn(move || {
                let _ = pipe.write_all(input.as_bytes());
                let _ = pipe.flush();
            });
        }
    }

    let stdout = child.stdout.take().map(spawn_capture);
    let stderr = child.stderr.take().map(spawn_capture);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    #[cfg(windows)]
                    let _ = job.terminate();
                    let _ = child.wait();
                    drop(stdout.map(join_capture));
                    drop(stderr.map(join_capture));
                    return ProcessRun::TimedOut;
                }
                thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(POLL_INTERVAL),
                );
            }
            Err(error) => {
                let _ = child.kill();
                #[cfg(windows)]
                let _ = job.terminate();
                let _ = child.wait();
                return ProcessRun::Failed {
                    detail: format!("could not wait for {program}: {error}"),
                };
            }
        }
    };

    ProcessRun::Completed {
        status,
        stdout: stdout.map(join_capture).unwrap_or_default(),
        stderr: stderr.map(join_capture).unwrap_or_default(),
    }
}

fn spawn_capture<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if buffer.len() < MAX_CAPTURED_BYTES {
                        let room = MAX_CAPTURED_BYTES - buffer.len();
                        buffer.extend_from_slice(&chunk[..read.min(room)]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

fn join_capture(handle: thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String, LandRefusal> {
    match run_program("git", cwd, args, None, LAND_GIT_TIMEOUT) {
        ProcessRun::Completed {
            status: Some(0),
            stdout,
            ..
        } => Ok(stdout),
        ProcessRun::Completed { stderr, .. } => Err(LandRefusal::Unavailable {
            detail: format!("git {} failed: {}", args.join(" "), first_meaningful_line(&stderr)),
        }),
        ProcessRun::TimedOut => Err(LandRefusal::Unavailable {
            detail: format!("git {} did not finish within {LAND_GIT_TIMEOUT:?}", args.join(" ")),
        }),
        ProcessRun::Failed { detail } => Err(LandRefusal::Unavailable { detail }),
    }
}
