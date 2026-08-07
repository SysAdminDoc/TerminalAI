//! Searching what a session printed — pure, no lock, no thread, no clock.
//!
//! The fleet keeps a 512 KB ring per session over an 8 MB rotating spool, and
//! until now nothing could query either: "where did that error print" across
//! twenty sessions was a manual scroll through twenty panes.
//!
//! # Escape sequences are removed before anything is matched
//!
//! Retained output is raw pty bytes, which is a rendered TUI: colours, cursor
//! moves, a status line redrawn a hundred times a second. Matching the needle
//! against those bytes directly fails in both directions. It misses — a word
//! the agent coloured is `error` with an SGR sequence somewhere inside it — and
//! it invents, because `31m` appears in every red thing ever printed and `2J`
//! in every screen clear. Neither is a match an operator would recognise, so
//! the sequences come out first and the search runs over what was legible.
//!
//! # Everything here is bounded
//!
//! A search reads megabytes per session across the whole fleet and returns the
//! result through a frame the daemon has to be able to carry. Hits per session,
//! characters per line and the total line count are all capped, and a truncated
//! result says so rather than looking like a complete one.

use crate::session::SessionId;

/// Hits returned for one session. Past this the count is still exact — it is
/// the excerpts that stop, because the operator has enough to go and look.
pub const MAX_HITS_PER_SESSION: usize = 50;
/// Longest excerpt kept for one matching line. A TUI redraw can put a whole
/// screen on one line.
pub const MAX_LINE_CHARS: usize = 300;
/// Shortest needle accepted. One character matches most of any transcript and
/// costs a full fleet read to say so.
pub const MIN_NEEDLE_CHARS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchQuery {
    pub needle: String,
    /// Default off. An operator searching for `error` means `Error` too, and
    /// the agent decides the casing, not them.
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    /// 1-based line number within the retained history that was searched.
    ///
    /// Not a line number in the session's whole output: the spool is a bounded
    /// tail, so the beginning of a long run is genuinely gone. Numbering from
    /// what was searched is the only honest option.
    pub line: usize,
    pub text: String,
    pub matches: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionMatches {
    pub id: SessionId,
    pub name: String,
    /// Every occurrence found, whether or not an excerpt was kept for it.
    pub total_matches: usize,
    pub hits: Vec<SearchHit>,
    /// True when more lines matched than [`MAX_HITS_PER_SESSION`] allowed.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("a search needs at least {MIN_NEEDLE_CHARS} characters")]
    NeedleTooShort,
}

impl SearchQuery {
    pub fn new(needle: impl Into<String>, case_sensitive: bool) -> Result<Self, SearchError> {
        let needle = needle.into();
        if needle.chars().count() < MIN_NEEDLE_CHARS {
            return Err(SearchError::NeedleTooShort);
        }
        Ok(Self {
            needle,
            case_sensitive,
        })
    }
}

/// What a terminal actually showed, with the sequences that drew it removed.
///
/// A deliberately small ECMA-48 subset — enough to make retained output
/// legible, not a second terminal emulator. `grid.rs` is the emulator; this is
/// a filter, and the difference matters because a filter that tried to apply
/// cursor movement would have to reconstruct a screen and would then be wrong
/// in ways a search cannot detect.
pub fn plain_text(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            0x1b => index = skip_escape(bytes, index, &mut out),
            b'\n' => {
                out.push(b'\n');
                index += 1;
            }
            b'\r' => {
                // A bare carriage return is a redraw returning to column zero,
                // which is a line break as far as reading goes. `\r\n` is one
                // break, not two.
                if bytes.get(index + 1) != Some(&b'\n') {
                    out.push(b'\n');
                }
                index += 1;
            }
            b'\t' => {
                out.push(b' ');
                index += 1;
            }
            // Remaining C0 controls and DEL drew nothing legible.
            0x00..=0x1f | 0x7f => index += 1,
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Consume one escape sequence starting at `start`, returning the index after
/// it. An unterminated sequence — the spool is a tail, so it can begin
/// mid-sequence — consumes the rest of the buffer rather than emitting the
/// fragment as text.
fn skip_escape(bytes: &[u8], start: usize, out: &mut Vec<u8>) -> usize {
    let Some(kind) = bytes.get(start + 1) else {
        return bytes.len();
    };
    match kind {
        // CSI: parameters and intermediates, then a final byte.
        b'[' => {
            let mut index = start + 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&byte) {
                    return index;
                }
            }
            bytes.len()
        }
        // OSC, DCS, SOS, PM, APC: a string terminated by BEL or ST.
        b']' | b'P' | b'X' | b'^' | b'_' => {
            let mut index = start + 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    return index + 1;
                }
                if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                    return index + 2;
                }
                index += 1;
            }
            bytes.len()
        }
        // `ESC c` is a full reset; treat it as a break so the text either side
        // does not run together into a match that was never on one line.
        b'c' => {
            out.push(b'\n');
            start + 2
        }
        // Two-character escapes, and character-set designators like `ESC ( B`.
        b'(' | b')' | b'*' | b'+' | b'#' | b'%' => start + 3,
        _ => start + 2,
    }
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_in(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

/// Search one session's retained output.
///
/// `bytes` is raw pty output; the caller does not pre-clean it, because the
/// cleaning rule belongs with the matching rule.
pub fn search_output(
    id: SessionId,
    name: impl Into<String>,
    bytes: &[u8],
    query: &SearchQuery,
) -> SessionMatches {
    let text = plain_text(bytes);
    let needle = if query.case_sensitive {
        query.needle.clone()
    } else {
        query.needle.to_lowercase()
    };
    let mut total_matches = 0;
    let mut hits = Vec::new();
    let mut truncated = false;
    // Matching and quoting are deliberately two different strings. The needle
    // is compared against a case-folded line, but the excerpt comes from the
    // original: an insensitive search that quoted the folded text would show
    // the operator `e0432` for output that said `E0432` — text the agent never
    // printed, in the one place they would go looking for it.
    //
    // The excerpt is the *cleaned* line, though, not the raw bytes: showing
    // those would hand back the escape sequences the match was found without.
    for (index, line) in text.lines().enumerate() {
        let folded;
        let haystack = if query.case_sensitive {
            line
        } else {
            folded = line.to_lowercase();
            folded.as_str()
        };
        let matches = count_in(haystack, &needle);
        if matches == 0 {
            continue;
        }
        // Counted before the cap, so the total stays exact even when the
        // excerpts stop. A count that silently stopped at fifty would answer
        // "how many times" with "at least fifty" while looking precise.
        total_matches += matches;
        if hits.len() >= MAX_HITS_PER_SESSION {
            truncated = true;
            continue;
        }
        hits.push(SearchHit {
            line: index + 1,
            text: truncate_chars(line, MAX_LINE_CHARS),
            matches,
        });
    }
    SessionMatches {
        id,
        name: name.into(),
        total_matches,
        hits,
        truncated,
    }
}

/// Take at most `limit` characters, never splitting one. Byte slicing a line
/// that ends mid-codepoint panics, and this is fed whatever an agent printed.
fn truncate_chars(line: &str, limit: usize) -> String {
    if line.chars().count() <= limit {
        return line.to_owned();
    }
    line.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(needle: &str) -> SearchQuery {
        SearchQuery::new(needle, false).expect("query")
    }

    fn hits(bytes: &[u8], needle: &str) -> SessionMatches {
        search_output(SessionId::new(1), "row", bytes, &query(needle))
    }

    #[test]
    fn colour_sequences_neither_hide_a_match_nor_become_one() {
        // Both halves matter. The agent colours its own errors, so searching
        // raw bytes misses them; and "31m" is in every red thing ever printed,
        // so searching raw bytes invents matches that are not text at all.
        let coloured = b"\x1b[31merror:\x1b[0m build failed\n";
        assert_eq!(plain_text(coloured), "error: build failed\n");
        assert_eq!(hits(coloured, "error").total_matches, 1);
        assert_eq!(hits(coloured, "31m").total_matches, 0);
        assert_eq!(hits(coloured, "0m b").total_matches, 0);
    }

    #[test]
    fn an_osc_title_is_not_searchable_text() {
        // A window title is not output; matching it would report a hit on a
        // line the operator cannot find by scrolling.
        let bytes = b"\x1b]0;error in project\x07ok\n";
        assert_eq!(plain_text(bytes), "ok\n");
        assert_eq!(hits(bytes, "error").total_matches, 0);
    }

    #[test]
    fn an_osc_terminated_by_st_is_consumed_whole() {
        let bytes = b"\x1b]8;;https://example.invalid/error\x1b\\link\n";
        assert_eq!(plain_text(bytes), "link\n");
    }

    #[test]
    fn a_sequence_cut_off_by_the_spool_boundary_emits_no_fragment() {
        // The disk tier is a byte tail, so a read can begin or end mid-escape.
        // A fragment rendered as text would be a match nobody can explain.
        assert_eq!(plain_text(b"ok\x1b[38;2;255"), "ok");
        assert_eq!(plain_text(b"\x1b]0;unterminated title"), "");
    }

    #[test]
    fn a_redraw_does_not_join_two_lines_into_one_match() {
        // A status line rewritten in place separates its versions with a bare
        // carriage return. Treating that as nothing would let "ab" match the
        // seam between "…a" and "b…".
        let bytes = b"loading a\rb done\n";
        assert_eq!(plain_text(bytes), "loading a\nb done\n");
        assert_eq!(hits(bytes, "ab").total_matches, 0);
        assert_eq!(hits(bytes, "loading").total_matches, 1);
    }

    #[test]
    fn crlf_is_one_line_break_not_two() {
        assert_eq!(plain_text(b"one\r\ntwo\r\n"), "one\ntwo\n");
        assert_eq!(plain_text(b"one\r\ntwo\r\n").lines().count(), 2);
    }

    #[test]
    fn a_match_reports_its_line_and_how_many_times_it_appeared() {
        let bytes = b"clean\nerror error\nclean\nerror\n";
        let found = hits(bytes, "error");
        assert_eq!(found.total_matches, 3);
        assert_eq!(found.hits.len(), 2, "two lines matched, three occurrences");
        assert_eq!(found.hits[0].line, 2);
        assert_eq!(found.hits[0].matches, 2);
        assert_eq!(found.hits[1].line, 4);
        assert!(!found.truncated);
    }

    #[test]
    fn case_is_ignored_by_default_and_respected_on_request() {
        let bytes = b"Error: one\nerror: two\n";
        assert_eq!(hits(bytes, "error").total_matches, 2);
        let exact = SearchQuery::new("Error", true).expect("query");
        assert_eq!(
            search_output(SessionId::new(1), "row", bytes, &exact).total_matches,
            1
        );
    }

    #[test]
    fn the_count_stays_exact_after_the_excerpts_stop() {
        // A capped count would answer "how many times" with "at least fifty"
        // while looking like a precise number.
        let mut output = String::new();
        for _ in 0..(MAX_HITS_PER_SESSION + 20) {
            output.push_str("error here\n");
        }
        let found = hits(output.as_bytes(), "error");
        assert_eq!(found.total_matches, MAX_HITS_PER_SESSION + 20);
        assert_eq!(found.hits.len(), MAX_HITS_PER_SESSION);
        assert!(found.truncated, "a partial result must say it is partial");
    }

    #[test]
    fn a_redrawn_screen_on_one_line_is_truncated_without_splitting_a_character() {
        let line = format!("{}héllo error\n", "x".repeat(MAX_LINE_CHARS));
        let found = hits(line.as_bytes(), "error");
        assert_eq!(found.total_matches, 1);
        assert_eq!(found.hits[0].text.chars().count(), MAX_LINE_CHARS);
    }

    #[test]
    fn a_one_character_needle_is_refused_rather_than_read_from_the_whole_fleet() {
        assert_eq!(SearchQuery::new("e", false), Err(SearchError::NeedleTooShort));
        assert_eq!(SearchQuery::new("", false), Err(SearchError::NeedleTooShort));
        assert!(SearchQuery::new("er", false).is_ok());
    }

    #[test]
    fn the_excerpt_is_the_legible_line_not_the_bytes_that_drew_it() {
        let found = hits(b"\x1b[1;33mwarning\x1b[0m: unused\n", "warning");
        assert_eq!(found.hits[0].text, "warning: unused");
    }

    #[test]
    fn an_insensitive_search_quotes_what_the_agent_printed_not_the_folded_text() {
        // Matching folds case; quoting must not. An excerpt reading `e0432`
        // for output that said `E0432` is text the agent never printed, shown
        // in the one place the operator would then go looking for it.
        let found = hits(b"E0432: Unresolved Import\n", "e0432");
        assert_eq!(found.hits[0].text, "E0432: Unresolved Import");
        assert_eq!(found.total_matches, 1);
    }
}
