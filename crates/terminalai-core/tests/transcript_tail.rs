//! Transcript tailing against real files on disk.
//!
//! The three things tailing exists to learn — the agent's own session id, the
//! last thing it said, and what the run cost — are all wrong in a way the
//! operator cannot see if the reader mishandles an append, a truncation, or a
//! cumulative counter. These use real files because the failure modes are
//! filesystem-shaped.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use terminalai_core::agent::Agent;
use std::time::{Duration, SystemTime};

use terminalai_core::tail::{claude_project_slug, newest_transcript, TranscriptTail, MAX_LINE_BYTES};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "terminalai-tail-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    Scratch(dir)
}

fn append(path: &Path, lines: &[&str]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open");
    for line in lines {
        writeln!(file, "{line}").expect("write");
    }
    file.flush().expect("flush");
}

/// One Claude assistant record with usage, as the JSONL actually carries it.
fn claude_record(request: &str, text: &str, input: u64, output: u64) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"11111111-2222-3333-4444-555555555555","requestId":"{request}","message":{{"role":"assistant","model":"claude-opus-4-20250514","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
    )
}

#[test]
fn the_claude_project_slug_matches_the_documented_shape() {
    // Verified 2026-08-02 against a real ~/.claude/projects directory.
    assert_eq!(
        claude_project_slug(Path::new(r"C:\Users\me\repos\shop")),
        "C--Users-me-repos-shop"
    );
}

#[test]
fn a_session_with_no_transcript_reports_nothing_rather_than_zero() {
    let home = scratch("empty");
    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, Path::new(r"C:\repos\shop"), SystemTime::UNIX_EPOCH);
    assert!(!update.changed);
    assert_eq!(update.native_session_id, None);
    assert_eq!(update.last_message, None);
    // Zero requests is what "unknown" looks like here; the fleet header renders
    // that as an em dash rather than $0.00.
    assert_eq!(update.totals.requests, 0);
}

#[test]
fn the_native_session_id_last_message_and_cost_are_read() {
    let home = scratch("claude");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("11111111-2222-3333-4444-555555555555.jsonl");
    append(&file, &[&claude_record("req-1", "Ran the tests", 1000, 500)]);

    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert!(update.changed);
    assert_eq!(
        update.native_session_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555"),
        "this is what --resume takes; our own s0001 means nothing to the CLI"
    );
    assert_eq!(update.last_message.as_deref(), Some("Ran the tests"));
    assert_eq!(update.totals.input_tokens, 1000);
    assert_eq!(update.totals.output_tokens, 500);
    assert!(update.cost_usd > 0.0, "a priced model must produce a cost");
}

#[test]
fn a_flag_like_transcript_session_id_is_ignored() {
    let home = scratch("invalid-session-id");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(
        &file,
        &[r#"{"type":"system","sessionId":"--dangerously-skip-permissions"}"#],
    );

    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.native_session_id, None);
}

#[test]
fn a_second_poll_reads_only_what_was_appended() {
    let home = scratch("incremental");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(&file, &[&claude_record("req-1", "First", 100, 100)]);

    let mut tail = TranscriptTail::new(Agent::Claude);
    let first = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(first.totals.requests, 1);

    // Nothing new: the poll must be a no-op, not a re-read.
    let idle = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert!(!idle.changed);
    assert_eq!(idle.totals.requests, 1);

    append(&file, &[&claude_record("req-2", "Second", 200, 200)]);
    let second = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert!(second.changed);
    assert_eq!(second.totals.requests, 2);
    assert_eq!(second.totals.input_tokens, 300);
    assert_eq!(second.last_message.as_deref(), Some("Second"));
}

#[test]
fn a_repeated_request_id_is_counted_once() {
    // Claude rewrites a record when a turn is retried; summing both would
    // charge the operator twice for one request.
    let home = scratch("dedupe");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(
        &file,
        &[
            &claude_record("req-1", "Once", 100, 50),
            &claude_record("req-1", "Once", 100, 50),
        ],
    );
    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.totals.requests, 1);
    assert_eq!(update.totals.input_tokens, 100);
}

#[test]
fn a_cumulative_counter_replaces_rather_than_accumulates() {
    // Codex reports the session's running total on every turn. Adding those
    // multiplies the real figure by the number of turns — the defect the
    // roadmap called out before this could be wired to the fleet header.
    let home = scratch("cumulative");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("08")
        .join("03")
        .join("rollout-1.jsonl");
    let cumulative = |input: u64, output: u64| {
        format!(
            r#"{{"type":"event_msg","thread_id":"codex-thread-9","payload":{{"model":"gpt-5","usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
        )
    };
    append(
        &file,
        &[&cumulative(1000, 100), &cumulative(2500, 300), &cumulative(4000, 700)],
    );

    let mut tail = TranscriptTail::new(Agent::Codex);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    // The last figure, not the sum of all three (which would be 7500/1100).
    assert_eq!(update.totals.input_tokens, 4000);
    assert_eq!(update.totals.output_tokens, 700);
    assert_eq!(update.native_session_id.as_deref(), Some("codex-thread-9"));
}

#[test]
fn a_cumulative_counter_never_walks_backwards() {
    // A replayed earlier line, or a different session's file, would otherwise
    // reduce a total that can only grow.
    let home = scratch("backwards");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".codex")
        .join("sessions")
        .join("rollout-1.jsonl");
    let cumulative = |input: u64| {
        format!(r#"{{"type":"event_msg","payload":{{"usage":{{"input_tokens":{input},"output_tokens":0}}}}}}"#)
    };
    append(&file, &[&cumulative(5000), &cumulative(10)]);

    let mut tail = TranscriptTail::new(Agent::Codex);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.totals.input_tokens, 5000);
}

#[test]
fn a_half_written_line_is_left_for_the_next_poll() {
    // The agent is appending while we read. Parsing a partial record would
    // either fail or, worse, succeed against truncated JSON.
    let home = scratch("partial");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(&file, &[&claude_record("req-1", "Complete", 100, 100)]);
    // A record with no trailing newline: still being written.
    let mut handle = OpenOptions::new().append(true).open(&file).expect("open");
    write!(handle, "{}", claude_record("req-2", "Half", 900, 900)).expect("write");
    handle.flush().expect("flush");

    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.totals.requests, 1, "the partial record must not count");
    assert_eq!(update.last_message.as_deref(), Some("Complete"));

    // Once it is terminated, the next poll picks it up whole.
    writeln!(handle).expect("newline");
    handle.flush().expect("flush");
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.totals.requests, 2);
    assert_eq!(update.last_message.as_deref(), Some("Half"));
}

#[test]
fn a_truncated_file_starts_over_instead_of_splicing_records() {
    let home = scratch("truncated");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(&file, &[&claude_record("req-1", "Before", 500, 500)]);
    let mut tail = TranscriptTail::new(Agent::Claude);
    assert_eq!(tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH).totals.input_tokens, 500);

    // Rewritten shorter: the stored offset now points past the end, and reading
    // from it would splice the tail of one record onto the head of another.
    std::fs::write(&file, "").expect("truncate");
    append(&file, &[&claude_record("req-9", "After", 7, 7)]);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.totals.input_tokens, 7, "totals restart with the file");
    assert_eq!(update.last_message.as_deref(), Some("After"));
}

#[test]
fn a_deleted_transcript_is_rediscovered_rather_than_held_open() {
    let home = scratch("deleted");
    let cwd = Path::new(r"C:\repos\shop");
    let directory = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd));
    let first = directory.join("first.jsonl");
    append(&first, &[&claude_record("req-1", "First file", 100, 100)]);

    let mut tail = TranscriptTail::new(Agent::Claude);
    assert!(tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH).changed);
    assert_eq!(tail.path(), Some(first.as_path()));

    std::fs::remove_file(&first).expect("remove");
    // One poll notices the file is gone and unbinds…
    tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    let second = directory.join("second.jsonl");
    append(&second, &[&claude_record("req-2", "Second file", 42, 42)]);
    // …and the next finds the replacement.
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.last_message.as_deref(), Some("Second file"));
    assert_eq!(update.totals.input_tokens, 42);
}

#[test]
fn only_text_blocks_become_the_row_label() {
    // A tool call's arguments are not something to show as "what the agent
    // said", and can be arbitrarily large.
    let home = scratch("tool-blocks");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(
        &file,
        &[r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"rm -rf /"}},{"type":"text","text":"Checking the tests"}]}}"#],
    );
    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.last_message.as_deref(), Some("Checking the tests"));
}

#[test]
fn a_user_message_is_not_mistaken_for_the_agent_speaking() {
    let home = scratch("user-role");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(
        &file,
        &[r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"do the thing"}]}}"#],
    );
    let mut tail = TranscriptTail::new(Agent::Claude);
    assert_eq!(tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH).last_message, None);
}

#[test]
fn a_long_message_is_collapsed_to_one_row_sized_line() {
    let home = scratch("long");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    let long = "word ".repeat(500);
    append(
        &file,
        &[&format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"line one\nline two {long}"}}]}}}}"#
        )],
    );
    let mut tail = TranscriptTail::new(Agent::Claude);
    let message = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH).last_message.expect("a message");
    assert!(!message.contains('\n'), "a row is one line");
    assert!(message.chars().count() <= 401, "{}", message.chars().count());
    assert!(message.ends_with('…'));
}

#[test]
fn malformed_records_do_not_stop_the_ones_after_them() {
    let home = scratch("malformed");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    append(
        &file,
        &[
            "{not json at all",
            "",
            &claude_record("req-1", "Survived", 10, 10),
        ],
    );
    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.last_message.as_deref(), Some("Survived"));
    assert_eq!(update.totals.requests, 1);
}

#[test]
fn an_oversized_line_is_skipped_rather_than_wedging_the_tail() {
    let home = scratch("oversized");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .expect("open");
    output
        .write_all(&vec![b'x'; MAX_LINE_BYTES + 1])
        .expect("oversized line");
    writeln!(output).expect("oversized newline");
    writeln!(output, "{}", claude_record("after", "After oversized", 7, 7)).expect("record");
    output.flush().expect("flush");

    let mut tail = TranscriptTail::new(Agent::Claude);
    let _ = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.last_message.as_deref(), Some("After oversized"));
    assert_eq!(update.totals.input_tokens, 7);
}

#[test]
fn an_invalid_utf8_line_is_skipped_rather_than_wedging_the_tail() {
    let home = scratch("invalid-utf8");
    let cwd = Path::new(r"C:\repos\shop");
    let file = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd))
        .join("s.jsonl");
    std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .expect("open");
    output.write_all(b"{\xff}\n").expect("invalid record");
    writeln!(output, "{}", claude_record("after", "After invalid UTF-8", 9, 9)).expect("record");
    output.flush().expect("flush");

    let mut tail = TranscriptTail::new(Agent::Claude);
    let _ = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    let update = tail.poll(&home.0, cwd, SystemTime::UNIX_EPOCH);
    assert_eq!(update.last_message.as_deref(), Some("After invalid UTF-8"));
    assert_eq!(update.totals.input_tokens, 9);
}

#[test]
fn codex_rollouts_are_found_under_the_dated_tree() {
    let home = scratch("codex-discovery");
    let file = home
        .0
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("08")
        .join("03")
        .join("rollout-2026-08-03T10-00-00-abc.jsonl");
    append(&file, &["{}"]);
    assert_eq!(
        newest_transcript(Agent::Codex, &home.0, Path::new(r"C:\repos\shop")).as_deref(),
        Some(file.as_path())
    );
}

#[test]
fn a_new_session_does_not_adopt_an_earlier_run_s_transcript() {
    // The failure this prevents, observed live: a session launched into a
    // folder that already held transcripts bound to the newest existing one on
    // its first poll and reported that run's cost, token totals and resume id
    // as its own. A wrong number that looks exactly like a right one.
    let home = scratch("not-mine");
    let cwd = Path::new(r"C:\repos\shop");
    let directory = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd));
    let earlier = directory.join("earlier-run.jsonl");
    append(&earlier, &[&claude_record("old", "From an earlier run", 9999, 9999)]);

    // Clear the birth-time grace window, so the earlier file is unambiguously
    // older than this session rather than within a tick of it.
    std::thread::sleep(Duration::from_millis(250));
    let started_at = SystemTime::now();

    // The same floor is used on every poll, exactly as the daemon does it. An
    // earlier version of this test relaxed the floor on the second poll and so
    // depended on which of two files the filesystem happened to stamp later.
    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, started_at);
    assert_eq!(tail.path(), None, "no transcript belongs to this session yet");
    assert_eq!(update.totals.requests, 0);
    assert_eq!(update.native_session_id, None);
    assert_eq!(update.last_message, None);

    // Its own transcript appears, and only then is anything reported.
    let mine = directory.join("mine.jsonl");
    append(&mine, &[&claude_record("new", "Mine", 5, 5)]);
    let update = tail.poll(&home.0, cwd, started_at);
    assert_eq!(update.last_message.as_deref(), Some("Mine"));
    assert_eq!(update.totals.input_tokens, 5);
}

#[test]
fn a_concurrently_running_session_in_the_same_folder_is_not_adopted() {
    // The harder half of the same defect. Two sessions on one repo is a case
    // this app exists to support, and the older run is *still writing*, so its
    // modification time is newer than ours on every poll. Ranking on
    // modification time hands its cost and resume id to the new row forever;
    // only creation time distinguishes them.
    let home = scratch("concurrent");
    let cwd = Path::new(r"C:\repos\shop");
    let directory = home
        .0
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd));
    // Named so it sorts *after* ours: the tie-break on equal stamps must not be
    // what rescues this test, or it would pass with the defect still present.
    let theirs = directory.join("zz-already-running.jsonl");
    append(&theirs, &[&claude_record("t1", "Their first turn", 9999, 9999)]);

    std::thread::sleep(Duration::from_millis(250));
    let started_at = SystemTime::now();
    let mine = directory.join("mine.jsonl");
    append(&mine, &[&claude_record("m1", "My first turn", 5, 5)]);

    // The other session keeps working: its file becomes the most recently
    // modified thing in the directory, by a margin no clock tick can explain.
    std::thread::sleep(Duration::from_millis(250));
    append(&theirs, &[&claude_record("t2", "Their second turn", 9999, 9999)]);

    let mut tail = TranscriptTail::new(Agent::Claude);
    let update = tail.poll(&home.0, cwd, started_at);
    assert_eq!(tail.path(), Some(mine.as_path()));
    assert_eq!(update.last_message.as_deref(), Some("My first turn"));
    assert_eq!(update.totals.input_tokens, 5, "adopted another run's tokens");
}
