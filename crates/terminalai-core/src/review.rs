//! Read-only aggregation of pending Git work for the review surface.
//!
//! The daemon owns this query so every client sees the same working-tree
//! snapshot. It never stages, commits, resolves, or otherwise mutates a
//! repository; conflict markers are returned as data for the operator.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Agent, Session, SessionId};

pub const MAX_REVIEW_DIFF_BYTES: usize = 128 * 1024;

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
    pub reviewed: bool,
    pub diff: String,
    pub diff_truncated: bool,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum ReviewError {
    #[error("git command could not start: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {command} failed: {message}")]
    Command { command: String, message: String },
}

#[derive(Debug, Default)]
struct DiffStats {
    paths: BTreeSet<String>,
    additions: u64,
    deletions: u64,
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
        reviewed: session.reviewed,
        diff: String::new(),
        diff_truncated: false,
        error: None,
    };

    let result = collect_git_review(&session.cwd);
    match result {
        Ok((stats, conflicts, conflict_markers, diff, diff_truncated)) => {
            item.files_changed = stats.paths.len().max(conflicts.len());
            item.additions = stats.additions;
            item.deletions = stats.deletions;
            item.conflicts = conflicts;
            item.conflict_markers = conflict_markers;
            item.review_cost = (item.files_changed as u64 * 10)
                .saturating_add(item.additions)
                .saturating_add(item.deletions)
                .saturating_add(u64::from(item.conflict_markers) * 1_000);
            item.diff = diff;
            item.diff_truncated = diff_truncated;
        }
        Err(error) => item.error = Some(error.to_string()),
    }
    item
}

fn collect_git_review(
    cwd: &Path,
) -> Result<(DiffStats, Vec<String>, u32, String, bool), ReviewError> {
    let numstat = git(cwd, &["diff", "HEAD", "--no-renames", "--numstat", "--"])?;
    let diff = git(cwd, &["diff", "HEAD", "--no-ext-diff", "--unified=3", "--"])?;
    let status = git(cwd, &["status", "--porcelain=v1", "--untracked-files=no"])?;
    let stats = parse_numstat(&String::from_utf8_lossy(&numstat));
    let conflicts = parse_conflicts(&String::from_utf8_lossy(&status));
    let conflict_markers = count_conflict_markers(&String::from_utf8_lossy(&diff));
    let (diff, diff_truncated) = truncate_diff(&diff);
    Ok((stats, conflicts, conflict_markers, diff, diff_truncated))
}

fn git(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, ReviewError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ReviewError::Command {
            command: args.join(" "),
            message: if message.is_empty() {
                format!("exit status {}", output.status)
            } else {
                message
            },
        });
    }
    Ok(output.stdout)
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
    diff.lines()
        .filter(|line| {
            line.starts_with("+<<<<<<<")
                || line.starts_with("+=======")
                || line.starts_with("+>>>>>>>")
        })
        .count() as u32
}

fn truncate_diff(bytes: &[u8]) -> (String, bool) {
    if bytes.len() <= MAX_REVIEW_DIFF_BYTES {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let mut diff = String::from_utf8_lossy(&bytes[..MAX_REVIEW_DIFF_BYTES]).into_owned();
    diff.push_str("\n\n[diff truncated at 128 KiB]\n");
    (diff, true)
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
    }

    #[test]
    fn large_diffs_are_bounded_for_the_control_plane() {
        let (diff, truncated) = truncate_diff(&vec![b'x'; MAX_REVIEW_DIFF_BYTES + 1]);
        assert!(truncated);
        assert!(diff.len() < MAX_REVIEW_DIFF_BYTES + 64);
        assert!(diff.contains("diff truncated"));
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
}
