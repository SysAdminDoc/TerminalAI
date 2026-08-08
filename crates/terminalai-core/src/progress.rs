//! Progress an agent reports about itself, read from its own output.
//!
//! Agents that know how far along they are say so with ConEmu's `OSC 9;4`
//! sequence — `ESC ] 9 ; 4 ; state ; percent BEL` — which Windows Terminal,
//! ConEmu and the xterm.js progress addon all understand. Nothing else in this
//! supervisor can produce that number: hooks report *what* a session is doing,
//! and the transcript reports what it said, but neither carries a completion
//! percentage.
//!
//! It is read here, in the core, rather than in the focused pane's renderer.
//! The renderer exists once and the fleet is thirty rows deep, so decoding it
//! there would mean progress existed only for whichever session the operator
//! happened to be looking at. Every session's bytes already pass through this
//! process on their way to the ring and the grid.
//!
//! Absence is a value. A session that never emits the sequence has no progress,
//! and the taskbar shows no bar rather than a fabricated one — the same rule the
//! fleet row applies to a tool plan it was never given.

use serde::{Deserialize, Serialize};

/// The longest `OSC 9;4` payload worth keeping. The whole sequence is
/// `9;4;1;100` — ten bytes — and the cap is what stops an unterminated OSC
/// string from growing a buffer per session for as long as the daemon runs.
const MAX_PAYLOAD_BYTES: usize = 32;

/// What an agent last said about its own progress.
///
/// The percentage is optional on the states where the sequence makes it
/// optional: an agent can report that it failed without claiming to know how
/// far it got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskProgress {
    /// A definite share of the work, 0–100.
    Value { percent: u8 },
    /// Something went wrong. Windows paints the bar red.
    Error { percent: Option<u8> },
    /// Working, with no idea how far along. Windows paints a scrolling bar.
    Indeterminate,
    /// Stopped for now — the agent is waiting on something. Windows paints the
    /// bar yellow.
    Paused { percent: Option<u8> },
}

impl TaskProgress {
    /// The share of the work reported, when one was.
    pub fn percent(&self) -> Option<u8> {
        match self {
            Self::Value { percent } => Some(*percent),
            Self::Error { percent } | Self::Paused { percent } => *percent,
            Self::Indeterminate => None,
        }
    }
}

/// One `OSC 9;4` report, as read from the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressReport {
    /// `state 0` — the agent withdrew its progress. Distinct from never having
    /// reported any, because it has to overwrite what is on screen.
    Cleared,
    Set(TaskProgress),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    /// Saw `ESC`.
    Escape,
    /// Inside an OSC string, accumulating its payload.
    Osc,
    /// Inside an OSC string and saw `ESC`, which terminates it if `\` follows.
    OscEscape,
}

/// Reads `OSC 9;4` out of a session's output, one pty read at a time.
///
/// Stateful on purpose: a pty hands over whatever bytes were ready, so the
/// sequence routinely arrives split across two reads. A scanner that only
/// matched within one chunk would drop reports at random and look like an agent
/// that reports progress intermittently.
#[derive(Debug, Default)]
pub struct ProgressScanner {
    state: ScanState,
    payload: Vec<u8>,
    /// The payload ran past the cap, so whatever it was is not a progress
    /// report and must not be parsed from a truncated prefix.
    overflow: bool,
}

impl ProgressScanner {
    /// Feed a chunk and report the last progress report it contained.
    ///
    /// The last, not the first: a chunk can carry a whole run of updates, and
    /// only the final one describes the session now.
    pub fn advance(&mut self, bytes: &[u8]) -> Option<ProgressReport> {
        let mut latest = None;
        for &byte in bytes {
            if let Some(report) = self.step(byte) {
                latest = Some(report);
            }
        }
        latest
    }

    /// Forget any partly-read sequence. Used when a session's output is
    /// replayed from scratch, where a half-read escape from the live stream
    /// would otherwise splice onto the first bytes of the replay.
    pub fn reset(&mut self) {
        self.state = ScanState::Ground;
        self.payload.clear();
        self.overflow = false;
    }

    fn step(&mut self, byte: u8) -> Option<ProgressReport> {
        const ESC: u8 = 0x1b;
        const BEL: u8 = 0x07;
        const CAN: u8 = 0x18;
        const SUB: u8 = 0x1a;
        match self.state {
            ScanState::Ground => {
                if byte == ESC {
                    self.state = ScanState::Escape;
                }
                None
            }
            ScanState::Escape => {
                match byte {
                    b']' => {
                        self.state = ScanState::Osc;
                        self.payload.clear();
                        self.overflow = false;
                    }
                    // `ESC ESC` is still an escape beginning; anything else is
                    // some other sequence this scanner does not care about.
                    ESC => {}
                    _ => self.state = ScanState::Ground,
                }
                None
            }
            ScanState::Osc => match byte {
                BEL => self.finish(),
                ESC => {
                    self.state = ScanState::OscEscape;
                    None
                }
                // Both abort the sequence outright, so what was accumulated is
                // not a report and must not be parsed.
                CAN | SUB => {
                    self.reset();
                    None
                }
                _ => {
                    if self.payload.len() < MAX_PAYLOAD_BYTES {
                        self.payload.push(byte);
                    } else {
                        self.overflow = true;
                    }
                    None
                }
            },
            ScanState::OscEscape => match byte {
                b'\\' => self.finish(),
                // An `ESC` that did not start `ESC \` ends the string anyway —
                // it is the start of the next sequence, not payload.
                _ => {
                    let report = self.finish();
                    if byte == ESC {
                        self.state = ScanState::Escape;
                    }
                    report
                }
            },
        }
    }

    fn finish(&mut self) -> Option<ProgressReport> {
        let payload = std::mem::take(&mut self.payload);
        let overflow = self.overflow;
        self.reset();
        if overflow {
            return None;
        }
        parse_report(&payload)
    }
}

/// Decode one OSC payload, which is only a progress report if it says so.
fn parse_report(payload: &[u8]) -> Option<ProgressReport> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut fields = text.split(';');
    if fields.next()? != "9" || fields.next()? != "4" {
        return None;
    }
    let state = fields.next()?;
    // Absent, empty or unreadable is not zero. An agent that sent a malformed
    // report has not told us anything, and inventing 0% would paint an empty
    // bar over whatever the session last actually said.
    let percent = fields.next().and_then(parse_percent);
    match state {
        "0" => Some(ProgressReport::Cleared),
        // A value state with no readable number is the one case that cannot be
        // honoured: there is nothing to show.
        "1" => percent.map(|percent| ProgressReport::Set(TaskProgress::Value { percent })),
        "2" => Some(ProgressReport::Set(TaskProgress::Error { percent })),
        "3" => Some(ProgressReport::Set(TaskProgress::Indeterminate)),
        "4" => Some(ProgressReport::Set(TaskProgress::Paused { percent })),
        _ => None,
    }
}

fn parse_percent(field: &str) -> Option<u8> {
    let value: u32 = field.parse().ok()?;
    // Clamped rather than refused: an agent that says 150 means "done", and
    // dropping the report would leave the bar wherever it was.
    Some(value.min(100) as u8)
}

/// One taskbar's worth of progress, derived from every session reporting any.
///
/// The window has one progress bar and the fleet has as many answers as it has
/// agents, so the rule has to say what happens when they disagree — and the
/// answer is never to pick one or average them, because both invent a number no
/// agent reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FleetProgress {
    pub status: ProgressStatus,
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressStatus {
    Normal,
    Error,
    Paused,
    Indeterminate,
}

/// Reduce every session's report to the one bar the window can show.
///
/// - Nobody reporting means no bar at all, not an empty one.
/// - One session reporting is shown exactly as that session reported it.
/// - Several means the bar goes indeterminate: something is running and this
///   tool will not claim to know how far. An error anywhere outranks that,
///   because a failure is worth surfacing even when a second agent is fine.
pub fn fleet_progress<I>(reports: I) -> Option<FleetProgress>
where
    I: IntoIterator<Item = TaskProgress>,
{
    let mut reports = reports.into_iter();
    let first = reports.next()?;
    let Some(second) = reports.next() else {
        return Some(match first {
            TaskProgress::Value { percent } => FleetProgress {
                status: ProgressStatus::Normal,
                percent: Some(percent),
            },
            TaskProgress::Error { percent } => FleetProgress {
                status: ProgressStatus::Error,
                percent,
            },
            TaskProgress::Paused { percent } => FleetProgress {
                status: ProgressStatus::Paused,
                percent,
            },
            TaskProgress::Indeterminate => FleetProgress {
                status: ProgressStatus::Indeterminate,
                percent: None,
            },
        });
    };
    let any_error = [first, second]
        .into_iter()
        .chain(reports)
        .any(|report| matches!(report, TaskProgress::Error { .. }));
    Some(FleetProgress {
        status: if any_error {
            ProgressStatus::Error
        } else {
            ProgressStatus::Indeterminate
        },
        percent: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<ProgressReport> {
        let mut scanner = ProgressScanner::default();
        chunks
            .iter()
            .filter_map(|chunk| scanner.advance(chunk))
            .collect()
    }

    #[test]
    fn a_value_report_is_read_with_either_terminator() {
        assert_eq!(
            scan(&[b"\x1b]9;4;1;42\x07"]),
            vec![ProgressReport::Set(TaskProgress::Value { percent: 42 })]
        );
        assert_eq!(
            scan(&[b"\x1b]9;4;1;42\x1b\\"]),
            vec![ProgressReport::Set(TaskProgress::Value { percent: 42 })]
        );
    }

    #[test]
    fn every_state_the_sequence_defines_is_understood() {
        assert_eq!(scan(&[b"\x1b]9;4;0;0\x07"]), vec![ProgressReport::Cleared]);
        assert_eq!(
            scan(&[b"\x1b]9;4;2;80\x07"]),
            vec![ProgressReport::Set(TaskProgress::Error { percent: Some(80) })]
        );
        assert_eq!(
            scan(&[b"\x1b]9;4;2\x07"]),
            vec![ProgressReport::Set(TaskProgress::Error { percent: None })]
        );
        assert_eq!(
            scan(&[b"\x1b]9;4;3\x07"]),
            vec![ProgressReport::Set(TaskProgress::Indeterminate)]
        );
        assert_eq!(
            scan(&[b"\x1b]9;4;4;10\x07"]),
            vec![ProgressReport::Set(TaskProgress::Paused { percent: Some(10) })]
        );
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_one_report() {
        // The case that decides whether this is worth having: a pty hands over
        // whatever was ready, and a scanner that only matched within one chunk
        // would drop reports at random.
        assert_eq!(
            scan(&[b"\x1b]9", b";4;", b"1;", b"7", b"\x07"]),
            vec![ProgressReport::Set(TaskProgress::Value { percent: 7 })]
        );
        // Including a terminator torn in half.
        assert_eq!(
            scan(&[b"\x1b]9;4;1;7\x1b", b"\\"]),
            vec![ProgressReport::Set(TaskProgress::Value { percent: 7 })]
        );
    }

    #[test]
    fn only_the_last_report_in_a_chunk_describes_the_session() {
        let mut scanner = ProgressScanner::default();
        assert_eq!(
            scanner.advance(b"\x1b]9;4;1;10\x07building\x1b]9;4;1;90\x07"),
            Some(ProgressReport::Set(TaskProgress::Value { percent: 90 }))
        );
    }

    #[test]
    fn other_sequences_and_ordinary_text_report_nothing() {
        assert_eq!(scan(&[b"\x1b]0;a title\x07"]), vec![]);
        assert_eq!(scan(&[b"\x1b]9;a notification\x07"]), vec![]);
        assert_eq!(scan(&[b"\x1b]9;5;1;50\x07"]), vec![]);
        assert_eq!(scan(&[b"\x1b[31mred\x1b[0m 9;4;1;50"]), vec![]);
        // A state this build does not know is not a report. Reading it as one
        // would mean a future extension silently painted the wrong bar.
        assert_eq!(scan(&[b"\x1b]9;4;9;50\x07"]), vec![]);
        // A value state has to carry a value.
        assert_eq!(scan(&[b"\x1b]9;4;1\x07"]), vec![]);
        assert_eq!(scan(&[b"\x1b]9;4;1;abc\x07"]), vec![]);
    }

    #[test]
    fn an_out_of_range_value_is_clamped_rather_than_dropped() {
        // An agent that says 150 means it is done. Dropping the report would
        // leave the bar wherever it happened to be.
        assert_eq!(
            scan(&[b"\x1b]9;4;1;150\x07"]),
            vec![ProgressReport::Set(TaskProgress::Value { percent: 100 })]
        );
    }

    #[test]
    fn an_unterminated_sequence_neither_reports_nor_grows() {
        // A `ESC ]` with no terminator is an open string, and the bytes after it
        // arrive for as long as the session lives. The cap is what keeps a
        // buffer per session from growing with them.
        let mut scanner = ProgressScanner::default();
        for _ in 0..1000 {
            assert_eq!(scanner.advance(b"\x1b]9;4;1;50 and more text"), None);
        }
        assert!(
            scanner.payload.len() <= MAX_PAYLOAD_BYTES,
            "the payload buffer grew to {}",
            scanner.payload.len()
        );
        // And what it did accumulate is not parsed on the way out, because a
        // truncated prefix of an over-long string is not what the agent sent.
        assert_eq!(scanner.advance(b"\x07"), None);
    }

    #[test]
    fn an_aborted_sequence_reports_nothing() {
        assert_eq!(scan(&[b"\x1b]9;4;1;50\x18"]), vec![]);
        assert_eq!(scan(&[b"\x1b]9;4;1;50\x1a"]), vec![]);
    }

    #[test]
    fn one_reporter_reaches_the_taskbar_exactly_as_it_was_reported() {
        assert_eq!(fleet_progress([]), None, "no reports means no bar");
        assert_eq!(
            fleet_progress([TaskProgress::Value { percent: 30 }]),
            Some(FleetProgress {
                status: ProgressStatus::Normal,
                percent: Some(30),
            })
        );
        assert_eq!(
            fleet_progress([TaskProgress::Paused { percent: Some(30) }]),
            Some(FleetProgress {
                status: ProgressStatus::Paused,
                percent: Some(30),
            })
        );
        assert_eq!(
            fleet_progress([TaskProgress::Indeterminate]),
            Some(FleetProgress {
                status: ProgressStatus::Indeterminate,
                percent: None,
            })
        );
    }

    #[test]
    fn several_reporters_never_produce_a_number_nobody_reported() {
        // Averaging 20% and 80% would put the bar at 50%, which no agent said
        // and the operator cannot act on.
        assert_eq!(
            fleet_progress([
                TaskProgress::Value { percent: 20 },
                TaskProgress::Value { percent: 80 },
            ]),
            Some(FleetProgress {
                status: ProgressStatus::Indeterminate,
                percent: None,
            })
        );
        // A failure anywhere outranks it: one agent being fine does not make the
        // other one's error worth hiding.
        assert_eq!(
            fleet_progress([
                TaskProgress::Value { percent: 20 },
                TaskProgress::Value { percent: 80 },
                TaskProgress::Error { percent: None },
            ]),
            Some(FleetProgress {
                status: ProgressStatus::Error,
                percent: None,
            })
        );
    }
}
