//! Running one prompt across many projects.
//!
//! The fleet already knows which projects have open roadmap items and can hold
//! thirty sessions at once. This is the piece that connects them: pick a stored
//! prompt, pick the projects, and let the queue create a session per project as
//! the fleet has room.
//!
//! Distinct from broadcast, which sends a prompt to sessions that already
//! exist. This one *creates* them, which is why it is much more careful:
//!
//! - **A repository with uncommitted changes is flagged, not launched.** An
//!   agent let loose on a dirty tree mixes its work with the operator's, and
//!   the diff that results cannot be separated afterwards. The operator can
//!   override per entry, but never by default.
//! - **The prompt is delivered as a pty write, not as an argument.** These
//!   prompts run to several kilobytes of prose. A command line is the wrong
//!   place for that on any platform and an impossible one on Windows, where
//!   quoting mangles anything containing `&`, `^`, `|` or `%`.
//! - **Admission is the fleet's, not the queue's.** The queue asks for one slot
//!   at a time and stops asking when told no; it never decides for itself how
//!   many agents this machine can run.
//! - **Every outcome is recorded.** A queue that ran forty projects and reports
//!   only "done" is one the operator has to audit by hand, which is what they
//!   were trying to avoid.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::land::{run_program, ProcessRun};
use crate::session::SessionId;

/// Ceiling for the `git status` that decides whether a tree is clean. A
/// repository that cannot answer in this long is reported as unknown rather
/// than assumed clean.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Most projects one run may target. Above this it is not a queue, it is a
/// script, and the operator should say which projects they mean.
pub const MAX_ENTRIES: usize = 200;

/// How long a queued entry may wait for a fleet slot before it is given up on.
///
/// A queue with no deadline launches whatever was asked for hours ago whenever
/// a slot happens to free, and stale queued work is usually unwanted work — the
/// tree has moved, the prompt has been superseded, or the operator has already
/// done it by hand. pgbouncer bounds the same situation with `query_wait_timeout`
/// (120 s, then disconnect) and Google's SRE guidance recommends shedding the
/// oldest queued item under overload for exactly this reason.
///
/// Two hours rather than two minutes because the unit of work here is an agent
/// session, not a database query: a fleet of three can legitimately take an hour
/// to reach the twentieth project in a run the operator started deliberately.
pub const DEFAULT_WAIT_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);

/// Whether a project's working tree is safe to turn an agent loose in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TreeState {
    Clean,
    /// Uncommitted changes, with a few paths so the operator can recognise it.
    Dirty { files: Vec<String> },
    /// Git could not answer. Deliberately not "clean": treating an unknown tree
    /// as safe is the one mistake this check exists to prevent.
    Unknown { detail: String },
}

impl TreeState {
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }
}

/// Where one project's run has got to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryState {
    /// Waiting for a fleet slot.
    Pending,
    /// Held back because the working tree is not clean. Needs an explicit
    /// decision before it will run.
    Flagged { tree: TreeState },
    /// A session was created and the prompt was queued into it.
    Running { session: SessionId },
    /// The session finished.
    Done { session: SessionId },
    /// It could not be started, and why.
    Failed { detail: String },
    /// The operator withdrew it.
    Skipped,
    /// It waited longer than the run's deadline without ever getting a slot.
    ///
    /// Its own category rather than `Failed`: nothing went wrong, the fleet was
    /// simply busy for longer than the work was worth, and a run summary that
    /// calls that a failure sends the operator looking for a fault.
    Expired { waited_seconds: u64 },
}

impl EntryState {
    /// True while this entry still needs something to happen.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Pending | Self::Running { .. })
    }
}

/// One project in a run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkEntry {
    pub project: PathBuf,
    pub name: String,
    pub state: EntryState,
}

/// A prompt queued against a set of projects.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkQueue {
    /// Which stored prompt this run uses.
    pub prompt: String,
    pub entries: Vec<WorkEntry>,
    /// Set while the operator has stopped the queue starting anything new.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub started_at: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkQueueError {
    #[error("a run needs at least one project")]
    NoProjects,
    #[error("a run of {0} projects exceeds the limit of {MAX_ENTRIES}")]
    TooMany(usize),
    #[error("no project at {0} in this run")]
    Missing(PathBuf),
}

impl WorkQueue {
    /// Build a run over the given projects.
    ///
    /// Every project starts `Pending`; the tree check happens when the entry is
    /// about to run, not now, because a tree the operator cleans up in the
    /// meantime should not stay flagged from an hour ago.
    pub fn new(prompt: &str, projects: &[(String, PathBuf)]) -> Result<Self, WorkQueueError> {
        if projects.is_empty() {
            return Err(WorkQueueError::NoProjects);
        }
        if projects.len() > MAX_ENTRIES {
            return Err(WorkQueueError::TooMany(projects.len()));
        }
        Ok(Self {
            prompt: prompt.to_owned(),
            entries: projects
                .iter()
                .map(|(name, project)| WorkEntry {
                    project: project.clone(),
                    name: name.clone(),
                    state: EntryState::Pending,
                })
                .collect(),
            paused: false,
            started_at: Some(SystemTime::now()),
        })
    }

    /// The next project to start, if the queue should start one now.
    ///
    /// Insertion order is kept deliberately. Newest-first (LIFO) is the standard
    /// recommendation under sustained overload, and it was considered and
    /// rejected here: a run is a list the operator wrote in the order they meant,
    /// and reordering it silently would make a partial run cover a different set
    /// of projects than the top of the list suggests. The deadline below is what
    /// handles staleness instead, and it does so visibly.
    pub fn next_pending(&self) -> Option<&WorkEntry> {
        if self.paused {
            return None;
        }
        self.entries
            .iter()
            .find(|entry| matches!(entry.state, EntryState::Pending))
    }

    /// Give up on entries that have waited past `deadline`, returning how many.
    ///
    /// Measured from the run's start, which is when the operator asked for the
    /// work — not from when a slot was last checked. A paused run does not age
    /// out: the operator stopped it on purpose and is coming back to it.
    pub fn expire_stale(&mut self, deadline: Duration, now: SystemTime) -> usize {
        if self.paused {
            return 0;
        }
        let Some(started_at) = self.started_at else {
            return 0;
        };
        let Ok(waited) = now.duration_since(started_at) else {
            return 0;
        };
        if waited < deadline {
            return 0;
        }
        let waited_seconds = waited.as_secs();
        let mut expired = 0;
        for entry in &mut self.entries {
            // Only entries still waiting for a slot. A flagged entry is waiting
            // on the operator, not on the fleet, and a running one has already
            // had its slot.
            if matches!(entry.state, EntryState::Pending) {
                entry.state = EntryState::Expired { waited_seconds };
                expired += 1;
            }
        }
        expired
    }

    /// How many of this run's sessions are still going.
    pub fn running(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.state, EntryState::Running { .. }))
            .count()
    }

    /// True when nothing is left to start or wait for.
    pub fn is_finished(&self) -> bool {
        !self.entries.iter().any(|entry| entry.state.is_open())
    }

    pub fn set_state(&mut self, project: &Path, state: EntryState) -> Result<(), WorkQueueError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.project == project)
            .ok_or_else(|| WorkQueueError::Missing(project.to_path_buf()))?;
        entry.state = state;
        Ok(())
    }

    /// Mark the entry owned by a finished session as done.
    ///
    /// Session lifecycle events arrive independently of the queue, so the
    /// bridge may see an exit more than once (first as `Exited`, then as
    /// `SessionRemoved`). A repeated event is deliberately a no-op.
    pub fn finish_session(&mut self, session: &SessionId) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| {
            matches!(
                &entry.state,
                EntryState::Running { session: running } if running == session
            )
        }) else {
            return false;
        };
        entry.state = EntryState::Done {
            session: session.clone(),
        };
        true
    }

    /// Move a flagged entry back into the queue, having accepted the risk.
    ///
    /// Only ever from `Flagged`: an entry that failed for another reason is not
    /// retried by this, because "run it anyway" is a statement about a dirty
    /// tree, not a general retry.
    pub fn approve_flagged(&mut self, project: &Path) -> Result<(), WorkQueueError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.project == project)
            .ok_or_else(|| WorkQueueError::Missing(project.to_path_buf()))?;
        if matches!(entry.state, EntryState::Flagged { .. }) {
            entry.state = EntryState::Pending;
        }
        Ok(())
    }

    /// A one-line count of how the run went.
    pub fn outcome(&self) -> WorkOutcome {
        let mut outcome = WorkOutcome::default();
        for entry in &self.entries {
            match entry.state {
                EntryState::Pending => outcome.pending += 1,
                EntryState::Flagged { .. } => outcome.flagged += 1,
                EntryState::Running { .. } => outcome.running += 1,
                EntryState::Done { .. } => outcome.done += 1,
                EntryState::Failed { .. } => outcome.failed += 1,
                EntryState::Skipped => outcome.skipped += 1,
                EntryState::Expired { .. } => outcome.expired += 1,
            }
        }
        outcome
    }
}

/// What happened across a run. Every category is reported, including the ones
/// that did nothing — a queue that ran forty projects and says only "done" is
/// one the operator has to audit by hand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkOutcome {
    pub pending: usize,
    pub flagged: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Waited past the run's deadline without ever getting a fleet slot. Counted
    /// apart from `failed`: nothing went wrong, the fleet was busy.
    #[serde(default)]
    pub expired: usize,
}

/// Whether a repository has uncommitted changes.
///
/// `--porcelain` because its output is a stable contract; the human-readable
/// form changes between Git versions and is localized.
pub fn tree_state(project: &Path) -> TreeState {
    match run_program(
        "git",
        project,
        &["status", "--porcelain", "--untracked-files=normal"],
        None,
        GIT_TIMEOUT,
    ) {
        ProcessRun::Completed {
            status: Some(0),
            stdout,
            ..
        } => {
            let files: Vec<String> = stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                // Enough to recognise the repository, not the whole diff.
                .take(10)
                .map(|line| line.trim().to_owned())
                .collect();
            if files.is_empty() {
                TreeState::Clean
            } else {
                TreeState::Dirty { files }
            }
        }
        ProcessRun::Completed { stderr, .. } => TreeState::Unknown {
            detail: stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("git status failed")
                .to_owned(),
        },
        ProcessRun::TimedOut => TreeState::Unknown {
            detail: format!("git status did not finish within {GIT_TIMEOUT:?}"),
        },
        ProcessRun::Failed { detail } => TreeState::Unknown { detail },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projects(names: &[&str]) -> Vec<(String, PathBuf)> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), PathBuf::from(format!(r"C:\repos\{name}"))))
            .collect()
    }

    #[test]
    fn a_run_starts_with_every_project_pending() {
        let queue = WorkQueue::new("drain", &projects(&["a", "b"])).expect("queue");
        assert_eq!(queue.entries.len(), 2);
        assert!(queue.entries.iter().all(|entry| entry.state == EntryState::Pending));
        assert_eq!(queue.next_pending().expect("next").name, "a");
    }

    #[test]
    fn a_run_over_no_projects_is_refused() {
        assert_eq!(WorkQueue::new("drain", &[]), Err(WorkQueueError::NoProjects));
    }

    #[test]
    fn a_run_is_bounded() {
        let many: Vec<(String, PathBuf)> = (0..MAX_ENTRIES + 1)
            .map(|index| (format!("p{index}"), PathBuf::from(format!("/p{index}"))))
            .collect();
        assert!(matches!(
            WorkQueue::new("drain", &many),
            Err(WorkQueueError::TooMany(_))
        ));
    }

    #[test]
    fn a_paused_queue_starts_nothing_new() {
        let mut queue = WorkQueue::new("drain", &projects(&["a"])).expect("queue");
        queue.paused = true;
        assert!(queue.next_pending().is_none());
        // But it is not finished — the work is still waiting.
        assert!(!queue.is_finished());
    }

    #[test]
    fn a_flagged_entry_is_not_started_until_it_is_approved() {
        // An agent let loose on a dirty tree mixes its work with the
        // operator's, and the diff cannot be separated afterwards.
        let mut queue = WorkQueue::new("drain", &projects(&["a", "b"])).expect("queue");
        let dirty = PathBuf::from(r"C:\repos\a");
        queue
            .set_state(
                &dirty,
                EntryState::Flagged {
                    tree: TreeState::Dirty {
                        files: vec!["M src/main.rs".into()],
                    },
                },
            )
            .expect("flag");
        assert_eq!(queue.next_pending().expect("next").name, "b");

        queue.approve_flagged(&dirty).expect("approve");
        assert_eq!(queue.next_pending().expect("next").name, "a");
    }

    #[test]
    fn approving_something_that_failed_for_another_reason_does_not_retry_it() {
        // "Run it anyway" is a statement about a dirty tree, not a general
        // retry — a launch that failed because the agent is missing would
        // simply fail again.
        let mut queue = WorkQueue::new("drain", &projects(&["a"])).expect("queue");
        let project = PathBuf::from(r"C:\repos\a");
        queue
            .set_state(
                &project,
                EntryState::Failed {
                    detail: "agent not found".into(),
                },
            )
            .expect("fail");
        queue.approve_flagged(&project).expect("approve");
        assert!(matches!(queue.entries[0].state, EntryState::Failed { .. }));
    }

    #[test]
    fn a_run_is_finished_only_when_nothing_is_pending_or_running() {
        let mut queue = WorkQueue::new("drain", &projects(&["a", "b"])).expect("queue");
        assert!(!queue.is_finished());
        queue
            .set_state(
                Path::new(r"C:\repos\a"),
                EntryState::Done {
                    session: SessionId::new(1),
                },
            )
            .expect("done");
        assert!(!queue.is_finished());
        queue
            .set_state(Path::new(r"C:\repos\b"), EntryState::Skipped)
            .expect("skip");
        assert!(queue.is_finished());
    }

    #[test]
    fn a_finished_session_is_done_and_exposes_the_next_pending_project() {
        let mut queue = WorkQueue::new("drain", &projects(&["a", "b"])).expect("queue");
        let session = SessionId::new(1);
        queue
            .set_state(
                Path::new(r"C:\repos\a"),
                EntryState::Running {
                    session: session.clone(),
                },
            )
            .expect("running");

        assert!(queue.finish_session(&session));
        assert!(matches!(queue.entries[0].state, EntryState::Done { .. }));
        assert_eq!(queue.next_pending().expect("next").name, "b");
        assert!(!queue.finish_session(&session), "duplicate exit changed state");
    }

    #[test]
    fn a_flagged_run_counts_as_finished_because_it_is_waiting_on_a_person() {
        // Nothing will happen without a decision, so the queue must stop
        // holding a slot open for it.
        let mut queue = WorkQueue::new("drain", &projects(&["a"])).expect("queue");
        queue
            .set_state(
                Path::new(r"C:\repos\a"),
                EntryState::Flagged {
                    tree: TreeState::Unknown {
                        detail: "not a repository".into(),
                    },
                },
            )
            .expect("flag");
        assert!(queue.is_finished());
        assert_eq!(queue.outcome().flagged, 1);
    }

    #[test]
    fn every_category_is_reported_including_the_ones_that_did_nothing() {
        // A queue that ran forty projects and reports only "done" is one the
        // operator has to audit by hand.
        let mut queue = WorkQueue::new("drain", &projects(&["a", "b", "c", "d"])).expect("queue");
        queue
            .set_state(
                Path::new(r"C:\repos\a"),
                EntryState::Done {
                    session: SessionId::new(1),
                },
            )
            .expect("state");
        queue
            .set_state(
                Path::new(r"C:\repos\b"),
                EntryState::Failed {
                    detail: "no agent".into(),
                },
            )
            .expect("state");
        queue
            .set_state(Path::new(r"C:\repos\c"), EntryState::Skipped)
            .expect("state");
        let outcome = queue.outcome();
        assert_eq!(outcome.done, 1);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(outcome.pending, 1);
    }

    #[test]
    fn setting_the_state_of_a_project_not_in_the_run_is_an_error() {
        let mut queue = WorkQueue::new("drain", &projects(&["a"])).expect("queue");
        assert!(queue
            .set_state(Path::new(r"C:\nowhere"), EntryState::Skipped)
            .is_err());
    }

    #[test]
    fn a_tree_that_git_cannot_describe_is_unknown_rather_than_clean() {
        // Treating an unreadable tree as safe is the one mistake this check
        // exists to prevent.
        let missing = std::env::temp_dir().join("terminalai-work-queue-not-a-repo");
        let _ = std::fs::create_dir_all(&missing);
        let state = tree_state(&missing);
        assert!(!state.is_clean(), "{state:?}");
        let _ = std::fs::remove_dir_all(&missing);
    }

    #[test]
    fn a_real_repository_is_read_as_clean_or_dirty_by_running_git() {
        // The unit tests above never run git. This one does, because "is this
        // tree safe to turn an agent loose in" is the check the whole feature
        // rests on, and a wrong answer here launches into someone's work.
        let repo = std::env::temp_dir().join(format!(
            "terminalai-tree-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).expect("dir");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git")
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "Work Queue Test"]);
        std::fs::write(repo.join("file.txt"), "committed\n").expect("seed");
        git(&["add", "file.txt"]);
        git(&["commit", "--quiet", "-m", "seed"]);
        assert_eq!(tree_state(&repo), TreeState::Clean, "a committed tree is clean");

        // A tracked edit.
        std::fs::write(repo.join("file.txt"), "edited\n").expect("edit");
        match tree_state(&repo) {
            TreeState::Dirty { files } => assert!(!files.is_empty(), "dirty with no files listed"),
            other => panic!("an edited tree read as {other:?}"),
        }

        // And an untracked file alone is enough: an agent would commit it.
        git(&["checkout", "--", "file.txt"]);
        std::fs::write(repo.join("scratch.txt"), "not committed").expect("untracked");
        assert!(
            !tree_state(&repo).is_clean(),
            "an untracked file read as a clean tree"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_run_survives_being_written_and_read_back() {
        let mut queue = WorkQueue::new("drain", &projects(&["a", "b"])).expect("queue");
        queue
            .set_state(
                Path::new(r"C:\repos\a"),
                EntryState::Running {
                    session: SessionId::new(3),
                },
            )
            .expect("state");
        let json = serde_json::to_string(&queue).expect("encode");
        let restored: WorkQueue = serde_json::from_str(&json).expect("decode");
        assert_eq!(restored, queue);
    }

    /// A queue with no deadline launches work queued hours ago the moment a slot
    /// frees, against a tree that has usually moved since.
    #[test]
    fn work_waiting_past_the_deadline_expires_rather_than_launching() {
        let mut queue = WorkQueue::new(
            "prompt",
            &[
                ("alpha".into(), PathBuf::from("/a")),
                ("beta".into(), PathBuf::from("/b")),
            ],
        )
        .expect("queue");
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        queue.started_at = Some(started);
        queue
            .set_state(Path::new("/b"), EntryState::Running { session: SessionId::new(1) })
            .expect("running entry");

        // Inside the deadline nothing moves.
        assert_eq!(queue.expire_stale(DEFAULT_WAIT_DEADLINE, started + Duration::from_secs(60)), 0);
        assert!(queue.next_pending().is_some());

        let expired = queue.expire_stale(
            DEFAULT_WAIT_DEADLINE,
            started + DEFAULT_WAIT_DEADLINE + Duration::from_secs(1),
        );
        assert_eq!(expired, 1, "only the entry still waiting for a slot");
        assert!(queue.next_pending().is_none(), "an expired entry must not launch");

        let outcome = queue.outcome();
        assert_eq!(outcome.expired, 1);
        // Its own category. Calling it a failure sends the operator looking for
        // a fault that does not exist.
        assert_eq!(outcome.failed, 0);
        // A running entry keeps its slot; it already got one.
        assert_eq!(outcome.running, 1);
    }

    #[test]
    fn a_paused_run_does_not_age_out() {
        let mut queue = WorkQueue::new("prompt", &[("alpha".into(), PathBuf::from("/a"))])
            .expect("queue");
        let started = SystemTime::UNIX_EPOCH;
        queue.started_at = Some(started);
        queue.paused = true;
        // The operator stopped it deliberately and is coming back to it; expiring
        // it while paused would punish them for pausing.
        assert_eq!(
            queue.expire_stale(DEFAULT_WAIT_DEADLINE, started + DEFAULT_WAIT_DEADLINE * 10),
            0
        );
    }
}
