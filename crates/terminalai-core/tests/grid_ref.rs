//! Conformance cases for the terminal grid.
//!
//! Each fixture under `fixtures/vt/` is a byte stream plus the grid a conforming
//! terminal must hold after replaying it. The expectations are derived from
//! ECMA-48 and DEC STD 070, **not** from this implementation's own output — an
//! expectation captured from the code under test only pins today's behaviour and
//! would ratify a bug as the standard.
//!
//! Alacritty's ref-test harness is the model (Apache-2.0): raw recording bytes
//! replayed in-process with no GUI and no pty, compared against a serialized
//! grid. `esctest2` is deliberately not adopted — it reads cells back with
//! DECRQCRA, which `vte` 0.15 does not dispatch, so this grid could never answer
//! it.
//!
//! Fixture format, one directive per line:
//!
//! ```text
//! rows 4
//! cols 20
//! why  <prose; repeatable. Say which rule decides the expectation.>
//! stream <bytes, with \e \r \n \t \\ and \xNN escapes; repeatable, concatenated>
//! cursor <row> <col>          (optional; zero-based, checked when present)
//! line <expected text>        (repeatable; exactly `rows` of them, in order)
//! ```
//!
//! `line` with no text is an empty row. Trailing blanks are not expressible
//! because `snapshot()` trims them, which is lossless for what it can report.

use std::path::{Path, PathBuf};

use terminalai_core::TerminalGrid;

struct Case {
    name: String,
    rows: u16,
    cols: u16,
    why: String,
    stream: Vec<u8>,
    cursor: Option<(u16, u16)>,
    lines: Vec<String>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("vt")
}

/// `\e`, `\r`, `\n`, `\t`, `\\` and `\xNN`. Anything else after a backslash is a
/// fixture bug rather than a literal, so it panics rather than silently
/// emitting the backslash — a mis-escaped stream would otherwise "pass" against
/// an expectation written for the sequence that was meant.
fn unescape(source: &str, case: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buffer = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('e') => out.push(0x1b),
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                let byte = u8::from_str_radix(&hex, 16)
                    .unwrap_or_else(|_| panic!("{case}: \\x needs two hex digits, got {hex:?}"));
                out.push(byte);
            }
            other => panic!("{case}: unknown escape \\{}", other.unwrap_or(' ')),
        }
    }
    out
}

fn parse(name: &str, text: &str) -> Case {
    let mut rows = None;
    let mut cols = None;
    let mut why = String::new();
    let mut stream = Vec::new();
    let mut cursor = None;
    let mut lines = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.trim().is_empty() || raw.starts_with('#') {
            continue;
        }
        let (directive, rest) = raw.split_once(' ').unwrap_or((raw, ""));
        let at = format!("{name}:{}", number + 1);
        match directive {
            "rows" => rows = Some(rest.trim().parse().expect("rows")),
            "cols" => cols = Some(rest.trim().parse().expect("cols")),
            "why" => {
                if !why.is_empty() {
                    why.push(' ');
                }
                why.push_str(rest.trim());
            }
            "stream" => stream.extend(unescape(rest, &at)),
            "cursor" => {
                let mut parts = rest.split_whitespace();
                let row = parts.next().expect("cursor row").parse().expect("row");
                let col = parts.next().expect("cursor col").parse().expect("col");
                cursor = Some((row, col));
            }
            // Expected text takes the same escapes as the stream, so a
            // combining mark or a wide character is written as the bytes it
            // actually is rather than as something invisible in the fixture.
            "line" => lines.push(
                String::from_utf8(unescape(rest, &at))
                    .unwrap_or_else(|_| panic!("{at}: expected line is not UTF-8")),
            ),
            other => panic!("{at}: unknown directive {other:?}"),
        }
    }
    let rows: u16 = rows.unwrap_or_else(|| panic!("{name}: no rows"));
    let cols: u16 = cols.unwrap_or_else(|| panic!("{name}: no cols"));
    assert_eq!(
        lines.len(),
        rows as usize,
        "{name}: declares {rows} rows but supplies {} line directives",
        lines.len()
    );
    assert!(!why.is_empty(), "{name}: every case states the rule it pins");
    Case {
        name: name.to_owned(),
        rows,
        cols,
        why,
        stream,
        cursor,
        lines,
    }
}

fn cases() -> Vec<Case> {
    let dir = fixture_dir();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "vt"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no fixtures in {}", dir.display());
    entries
        .iter()
        .map(|path| {
            let name = path
                .file_stem()
                .expect("fixture name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(path).expect("read fixture");
            parse(&name, &text)
        })
        .collect()
}

fn replay(case: &Case, chunk: usize) -> terminalai_core::TerminalGridSnapshot {
    let mut grid = TerminalGrid::new(case.rows, case.cols);
    for slice in case.stream.chunks(chunk.max(1)) {
        grid.advance(slice);
    }
    grid.snapshot()
}

#[test]
fn recorded_streams_match_the_grid_the_standard_requires() {
    for case in cases() {
        let snapshot = replay(&case, case.stream.len().max(1));
        assert_eq!(
            snapshot.lines, case.lines,
            "{}\n  rule: {}",
            case.name, case.why
        );
        if let Some((row, col)) = case.cursor {
            assert_eq!(
                (snapshot.cursor_row, snapshot.cursor_col),
                (row, col),
                "{} cursor\n  rule: {}",
                case.name,
                case.why
            );
        }
    }
}

#[test]
fn the_parser_is_indifferent_to_where_a_read_was_cut() {
    // A pty hands over whatever the last read happened to contain, so an escape
    // sequence arrives split as often as whole. Every chunk size that can split
    // a sequence in a different place must land on the same grid.
    for case in cases() {
        let whole = replay(&case, case.stream.len().max(1));
        for chunk in [1, 2, 3, 5, 7, 11] {
            let split = replay(&case, chunk);
            assert_eq!(
                split.lines, whole.lines,
                "{} differs when fed {chunk} bytes at a time",
                case.name
            );
            assert_eq!(
                (split.cursor_row, split.cursor_col),
                (whole.cursor_row, whole.cursor_col),
                "{} cursor differs when fed {chunk} bytes at a time",
                case.name
            );
        }
    }
}

#[test]
fn every_case_says_which_rule_it_pins() {
    // The corpus is only worth having if a failure tells the next reader what
    // the right answer is. A case with no rule is a snapshot of today's output.
    for case in cases() {
        assert!(
            case.why.len() > 40,
            "{}: `why` must state the rule, not name the case",
            case.name
        );
    }
}
