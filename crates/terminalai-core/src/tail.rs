//! Following a session's transcript on disk.
//!
//! Both CLIs write a JSONL record per event. Tailing those is the only way to
//! learn three things the pty cannot tell us: the agent's own session id (which
//! is what `--resume` needs), the last thing it actually said, and what the run
//! cost. The pty carries a rendered TUI; none of that survives rendering.
//!
//! Two properties matter more than completeness here:
//!
//! - **Reads are incremental.** A long session's transcript reaches tens of
//!   megabytes; re-reading it on every poll would cost more than the fleet it is
//!   describing. Each file is read from the offset the last poll stopped at.
//! - **A truncated or rewritten file resets rather than misreports.** If a file
//!   shrinks, the offset is stale and continuing from it would splice two
//!   unrelated records together, so the accumulator starts over.
//!
//! Locating the files is vendor-specific and documented at each function.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::agent::Agent;
use crate::launch::is_valid_resume_id;
use crate::transcript::{TranscriptAccumulator, UsageTotals};

/// Cap on one line handed to the JSON parser. A transcript line carrying a
/// pasted file can be large, and an unbounded read would let the file dictate
/// this process's memory.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// How many bytes one poll will consume from one file. A session that wrote a
/// burst is caught up over several polls rather than in one stall.
pub const MAX_POLL_BYTES: u64 = 8 * 1024 * 1024;
/// Longest last-message text retained for the row.
pub const MAX_SUMMARY_CHARS: usize = 400;

/// How a transcript reader chose its file. Kept visible in diagnostics so an
/// operator can tell an exact provider id from the compatibility heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptBinding {
    ExplicitSessionId,
    Heuristic,
}

/// What one poll learned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptUpdate {
    /// The agent's own session id, once it appears. This is what `--resume`
    /// takes; the fleet's `s0001` is ours and means nothing to the CLI.
    pub native_session_id: Option<String>,
    /// The last assistant text, trimmed for a fleet row.
    pub last_message: Option<String>,
    pub totals: UsageTotals,
    pub cost_usd: f64,
    /// Tokens occupying the model's context window as of the last request.
    ///
    /// Separate from `totals`, which sums every request the session ever made.
    /// Only one of the two can go over a context window — see
    /// [`crate::context`].
    pub context_tokens: Option<u64>,
    /// True when this poll saw anything new at all.
    pub changed: bool,
}

enum BoundedLine {
    Eof,
    Partial {
        bytes_read: usize,
        oversized: bool,
    },
    Complete {
        bytes: Vec<u8>,
        bytes_read: usize,
        oversized: bool,
    },
}

/// Consume one physical line without handing an unbounded record to the
/// parser. Bytes are retained only while the line is within the cap, so an
/// oversized or invalid-UTF-8 record can still be skipped without wedging the
/// file offset or allocating the entire pasted payload.
fn read_bounded_line(
    reader: &mut BufReader<std::fs::File>,
    max_bytes: usize,
) -> std::io::Result<BoundedLine> {
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    let mut bytes_read = 0usize;
    let mut oversized = false;

    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if bytes_read == 0 {
                Ok(BoundedLine::Eof)
            } else {
                Ok(BoundedLine::Partial {
                    bytes_read,
                    oversized,
                })
            };
        }

        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(chunk.len(), |index| index + 1);
        if !oversized {
            if bytes_read.saturating_add(consumed) <= max_bytes {
                bytes.extend_from_slice(&chunk[..consumed]);
            } else {
                oversized = true;
                bytes.clear();
            }
        }
        reader.consume(consumed);
        bytes_read = bytes_read.saturating_add(consumed);

        if newline.is_some() {
            return Ok(BoundedLine::Complete {
                bytes,
                bytes_read,
                oversized,
            });
        }
    }
}

/// Whether a Codex rollout's first record declares the session's working
/// directory. A rollout is not eligible for discovery until that record is
/// complete; binding to a file before its session metadata arrives recreates
/// the cross-project adoption this check is meant to prevent.
fn codex_rollout_matches_cwd(path: &Path, cwd: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let Ok(BoundedLine::Complete {
        bytes,
        oversized: false,
        ..
    }) = read_bounded_line(&mut reader, MAX_LINE_BYTES)
    else {
        return false;
    };
    let Ok(line) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return false;
    };
    if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
        return false;
    }
    let declared_cwd = value
        .get("payload")
        .and_then(|payload| payload.get("cwd"))
        .or_else(|| value.get("cwd"))
        .and_then(serde_json::Value::as_str);
    let Some(declared_cwd) = declared_cwd else {
        return false;
    };
    normalized_cwd(declared_cwd) == normalized_cwd(&cwd.to_string_lossy())
}

/// Compare Windows working directories without requiring the test path to
/// exist. Codex may write either slash style, and Windows paths are
/// case-insensitive even when the host running the unit test is not.
fn normalized_cwd(cwd: &str) -> String {
    let mut normalized = cwd.trim().replace('/', "\\").to_ascii_lowercase();
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized
}

/// Follows one session's transcript.
#[derive(Debug)]
pub struct TranscriptTail {
    agent: Agent,
    path: Option<PathBuf>,
    expected_session_id: Option<String>,
    binding: Option<TranscriptBinding>,
    offset: u64,
    accumulator: TranscriptAccumulator,
    native_session_id: Option<String>,
    last_message: Option<String>,
}

impl TranscriptTail {
    pub fn new(agent: Agent) -> Self {
        Self::with_session_id(agent, None)
    }

    pub fn with_session_id(agent: Agent, expected_session_id: Option<String>) -> Self {
        Self {
            agent,
            path: None,
            expected_session_id,
            binding: None,
            offset: 0,
            accumulator: TranscriptAccumulator::with_vendored_pricing(),
            native_session_id: None,
            last_message: None,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn binding(&self) -> Option<TranscriptBinding> {
        self.binding
    }

    /// Bind this tail to a specific file, skipping discovery.
    pub fn follow(&mut self, path: PathBuf) {
        if self.path.as_ref() != Some(&path) {
            self.path = Some(path);
            self.binding = None;
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.offset = 0;
        self.accumulator = TranscriptAccumulator::with_vendored_pricing();
    }

    /// Read whatever is new and fold it in.
    ///
    /// `home` is the user's home directory, injected so discovery is testable
    /// without writing into the real `~/.claude`. `not_before` is the session's
    /// start time: a transcript older than that belongs to a different run, and
    /// binding to it would attribute another session's cost and resume id to
    /// this row — worse than showing none.
    pub fn poll(&mut self, home: &Path, cwd: &Path, not_before: SystemTime) -> TranscriptUpdate {
        if self.path.is_none() {
            let explicit = self
                .expected_session_id
                .as_deref()
                .and_then(|id| transcript_for_session_id(self.agent, home, cwd, id));
            let used_explicit = explicit.is_some();
            let discovered = explicit.or_else(|| {
                (!supports_explicit_session_id(self.agent) || self.expected_session_id.is_none())
                    .then(|| newest_transcript_after(self.agent, home, cwd, not_before))
                    .flatten()
            });
            if let Some(path) = discovered {
                self.binding = Some(if used_explicit {
                    TranscriptBinding::ExplicitSessionId
                } else {
                    TranscriptBinding::Heuristic
                });
                tracing::debug!(
                    agent = ?self.agent,
                    path = %path.display(),
                    expected_session_id = ?self.expected_session_id,
                    binding = ?self.binding,
                    "bound transcript"
                );
                self.follow(path);
                // `follow` is a public explicit-path escape hatch and clears the
                // marker when it changes files; restore the discovery strategy.
                self.binding = Some(if used_explicit {
                    TranscriptBinding::ExplicitSessionId
                } else {
                    TranscriptBinding::Heuristic
                });
            }
        }
        let Some(path) = self.path.clone() else {
            return self.snapshot(false);
        };

        let Ok(metadata) = std::fs::metadata(&path) else {
            // The file vanished — a session directory cleaned up underneath us.
            // Drop the binding so the next poll rediscovers rather than holding
            // an offset into a path that no longer exists.
            self.path = None;
            self.reset();
            return self.snapshot(false);
        };
        let length = metadata.len();
        if length < self.offset {
            // Truncated or replaced. Continuing from a stale offset would splice
            // the tail of one record onto the head of another.
            self.reset();
        }
        if length == self.offset {
            return self.snapshot(false);
        }

        let Ok(file) = std::fs::File::open(&path) else {
            return self.snapshot(false);
        };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.offset)).is_err() {
            return self.snapshot(false);
        }

        let budget_end = self.offset.saturating_add(MAX_POLL_BYTES);
        let mut changed = false;
        while let Ok(line) = read_bounded_line(&mut reader, MAX_LINE_BYTES) {
            match line {
                BoundedLine::Eof => break,
                BoundedLine::Partial {
                    bytes_read,
                    oversized,
                } => {
                    // A short unterminated line is still being written. Keep
                    // the offset before it so the next poll sees the record
                    // whole. An oversized partial line cannot be safely
                    // parsed, so consume the bytes already seen and move on.
                    if oversized {
                        self.offset = self.offset.saturating_add(bytes_read as u64);
                    }
                    break;
                }
                BoundedLine::Complete {
                    bytes,
                    bytes_read,
                    oversized,
                } => {
                    self.offset = self.offset.saturating_add(bytes_read as u64);
                    if oversized {
                        if self.offset >= budget_end {
                            break;
                        }
                        continue;
                    }
                    let Ok(line) = std::str::from_utf8(&bytes) else {
                        if self.offset >= budget_end {
                            break;
                        }
                        continue;
                    };
                    if self.absorb(line) {
                        changed = true;
                    }
                    if self.offset >= budget_end {
                        break;
                    }
                }
            }
        }
        self.snapshot(changed)
    }

    /// Fold one record in. Returns true when it changed anything.
    fn absorb(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return false;
        }
        let mut changed = self.accumulator.ingest_line(trimmed).unwrap_or(false);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return changed;
        };
        if let Some(id) = native_session_id(&value) {
            if self.native_session_id.as_deref() != Some(id.as_str()) {
                self.native_session_id = Some(id);
                changed = true;
            }
        }
        if let Some(text) = assistant_text(&value) {
            let summary = summarise(&text);
            if !summary.is_empty() && self.last_message.as_deref() != Some(summary.as_str()) {
                self.last_message = Some(summary);
                changed = true;
            }
        }
        changed
    }

    fn snapshot(&self, changed: bool) -> TranscriptUpdate {
        TranscriptUpdate {
            native_session_id: self.native_session_id.clone(),
            last_message: self.last_message.clone(),
            totals: self.accumulator.totals(),
            cost_usd: self.accumulator.cost_usd(),
            context_tokens: self.accumulator.context_tokens(),
            changed,
        }
    }
}

/// Claude's project slug for a working directory.
///
/// Verified 2026-08-02: the directory name is the absolute path with `\`, `:`
/// and `.` each replaced by `-`. The transcript file's stem is the session UUID.
pub fn claude_project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '.' | '_' | ' ' => '-',
            c => c,
        })
        .collect()
}

/// The newest transcript for this agent and directory, if one exists.
pub fn newest_transcript(agent: Agent, home: &Path, cwd: &Path) -> Option<PathBuf> {
    newest_transcript_after(agent, home, cwd, SystemTime::UNIX_EPOCH)
}

/// The newest transcript written no earlier than `not_before`.
///
/// A session that has just launched has not written a transcript yet, and the
/// directory usually already holds files from earlier runs in the same folder.
/// Without the floor, the first poll binds to one of those and reports its cost,
/// its token totals and its resume id against the new session — a wrong number
/// that looks exactly like a right one.
pub fn newest_transcript_after(
    agent: Agent,
    home: &Path,
    cwd: &Path,
    not_before: SystemTime,
) -> Option<PathBuf> {
    match agent {
        Agent::Claude => newest_in(
            &home
                .join(".claude")
                .join("projects")
                .join(claude_project_slug(cwd)),
            "jsonl",
            not_before,
        ),
        // Codex rollouts are filed by date rather than by project. Candidates
        // are opened and filtered by their first session-meta record before the
        // newest matching rollout is selected.
        Agent::Codex => newest_codex_under(
            &home.join(".codex").join("sessions"),
            "jsonl",
            4,
            cwd,
            not_before,
        ),
    }
}

fn supports_explicit_session_id(agent: Agent) -> bool {
    matches!(agent, Agent::Claude)
}

/// Claude's documented `--session-id` is also the JSONL filename. This path is
/// deliberately checked before any directory ranking; a same-folder session
/// cannot steal it by being newer or more recently modified.
fn transcript_for_session_id(
    agent: Agent,
    home: &Path,
    cwd: &Path,
    session_id: &str,
) -> Option<PathBuf> {
    if !is_valid_resume_id(session_id) {
        return None;
    }
    let path = match agent {
        Agent::Claude => home
            .join(".claude")
            .join("projects")
            .join(claude_project_slug(cwd))
            .join(format!("{session_id}.jsonl")),
        Agent::Codex => return None,
    };
    path.is_file().then_some(path)
}

/// Slack allowed below the birth-time floor.
///
/// A file's creation stamp comes from the coarse system clock — ~15.6 ms on
/// Windows — while `SystemTime::now()` is precise, so a transcript created
/// immediately *after* the session started can carry a stamp fractionally
/// before it. With no slack the session would reject its own transcript and
/// never bind to anything: silent, permanent, and worse than the ambiguity the
/// floor removes. Two deliberate launches into one folder this close together
/// is not a case that occurs outside a test.
const BIRTH_GRACE: Duration = Duration::from_millis(100);

/// When a file came into existence.
///
/// Discovery ranks on creation, never on modification. A session starting in a
/// folder where another session is *already running* sees that run's transcript
/// being appended right now, so its modification time is newer than the file we
/// are looking for — ranking on modification hands the older run's cost, token
/// totals and resume id to the new row. Creation time is the only stamp that
/// says which run a file belongs to.
///
/// `created` is unsupported on some filesystems; modification time is the
/// fallback, which is the previous behaviour rather than dropping the file.
fn birth_time(metadata: &std::fs::Metadata) -> Option<SystemTime> {
    metadata.created().or_else(|_| metadata.modified()).ok()
}

fn newest_in(directory: &Path, extension: &str, not_before: SystemTime) -> Option<PathBuf> {
    let floor = not_before.checked_sub(BIRTH_GRACE).unwrap_or(not_before);
    let entries = std::fs::read_dir(directory).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(extension) {
            continue;
        }
        let Some(born) = entry.metadata().ok().as_ref().and_then(birth_time) else {
            continue;
        };
        if born < floor {
            continue;
        }
        // Ties are broken by path so the choice is reproducible. Two files born
        // in the same clock tick are genuinely ambiguous; picking by directory
        // enumeration order made the same inputs answer differently on
        // different runs, which is how this rule's own test came to be flaky.
        let better = best
            .as_ref()
            .is_none_or(|(seen, kept)| born > *seen || (born == *seen && path > *kept));
        if better {
            best = Some((born, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Bounded recursive search. Codex nests rollouts `YYYY/MM/DD/`, so the depth
/// limit is the shape of that tree plus one, not a guess. Each candidate is
/// checked before it can win, so a newer rollout from another directory does
/// not hide the newest rollout that belongs to this session.
fn newest_codex_under(
    root: &Path,
    extension: &str,
    max_depth: usize,
    cwd: &Path,
    not_before: SystemTime,
) -> Option<PathBuf> {
    let floor = not_before.checked_sub(BIRTH_GRACE).unwrap_or(not_before);
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let mut queue = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = queue.pop() {
        if depth > max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                queue.push((entry.path(), depth + 1));
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some(extension) {
                continue;
            }
            let Some(born) = entry.metadata().ok().as_ref().and_then(birth_time) else {
                continue;
            };
            if born < floor || !codex_rollout_matches_cwd(&path, cwd) {
                continue;
            }
            let better = best
                .as_ref()
                .is_none_or(|(seen, kept)| born > *seen || (born == *seen && path > *kept));
            if better {
                best = Some((born, path));
            }
        }
    }

    best.map(|(_, path)| path)
}

/// The agent's own session id, wherever it hides in this record.
fn native_session_id(value: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 4] = ["sessionId", "session_id", "threadId", "thread_id"];
    fn walk(value: &serde_json::Value, depth: usize) -> Option<String> {
        if depth > 4 {
            return None;
        }
        let object = value.as_object()?;
        for key in KEYS {
            if let Some(id) = object.get(key).and_then(serde_json::Value::as_str) {
                let id = id.trim();
                if is_valid_resume_id(id) {
                    return Some(id.to_owned());
                }
            }
        }
        object.values().find_map(|child| walk(child, depth + 1))
    }
    walk(value, 0)
}

/// The assistant's text from one record, if it carried any.
///
/// Both vendors nest content blocks; only `text` blocks are read, so a tool
/// call's arguments never become a row label.
fn assistant_text(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let role_is_assistant = |value: &serde_json::Value| {
        value
            .get("role")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|role| role == "assistant")
    };
    let message = object
        .get("message")
        .filter(|message| role_is_assistant(message))
        .or_else(|| Some(value).filter(|value| role_is_assistant(value)))?;

    match message.get("content") {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(serde_json::Value::Array(blocks)) => {
            let text: Vec<&str> = blocks
                .iter()
                .filter(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                })
                .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                .collect();
            (!text.is_empty()).then(|| text.join(" "))
        }
        _ => None,
    }
}

/// Collapse a message to one line short enough for a 28px row.
fn summarise(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_SUMMARY_CHARS {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(MAX_SUMMARY_CHARS).collect();
    out.push('…');
    out
}

/// One tail per session, so the registry keeps a single map rather than a field
/// per concern.
#[derive(Debug, Default)]
pub struct TranscriptTails {
    tails: HashMap<String, TranscriptTail>,
}

impl TranscriptTails {
    pub fn poll(
        &mut self,
        session_id: &str,
        agent: Agent,
        home: &Path,
        cwd: &Path,
        not_before: SystemTime,
        expected_session_id: Option<&str>,
    ) -> TranscriptUpdate {
        self.tails
            .entry(session_id.to_owned())
            .or_insert_with(|| {
                TranscriptTail::with_session_id(
                    agent,
                    expected_session_id.map(str::to_owned),
                )
            })
            .poll(home, cwd, not_before)
    }

    pub fn forget(&mut self, session_id: &str) {
        self.tails.remove(session_id);
    }

    pub fn len(&self) -> usize {
        self.tails.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tails.is_empty()
    }
}
