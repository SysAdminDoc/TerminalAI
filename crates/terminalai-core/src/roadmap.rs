//! What each known project still has queued.
//!
//! With a registered root the fleet knows about a few hundred repositories.
//! "Which of them still has work waiting" is then a question worth answering at
//! a glance, and the answer is already written down: most of these projects
//! carry a `ROADMAP.md` with checklist items in it.
//!
//! Two distinctions do all the work here, and both are the difference between a
//! number and a *true* number:
//!
//! - **"No roadmap" is not "no work".** A project without the file is unknown,
//!   not finished. Reporting it as zero open items would sort it beside a
//!   project that genuinely has none.
//! - **"A roadmap with no checkboxes" is not "an empty roadmap".** Plenty of
//!   projects write their roadmap as prose or plain bullets. Counting zero
//!   there claims the work is done, when in truth this parser cannot tell.
//!
//! Staleness is the file's own modification time rather than a Git log lookup.
//! A `git log` per project is a process per project, and the launcher would pay
//! it every time it opened for a number that only needs to be approximate.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Files treated as a project's roadmap, in preference order.
pub const ROADMAP_NAMES: [&str; 2] = ["ROADMAP.md", "roadmap.md"];

/// Most of a roadmap that will be read. A file larger than this is not a
/// checklist, and the scan runs across every known project at once.
pub const MAX_ROADMAP_BYTES: u64 = 1024 * 1024;

/// Longest preview kept for the next open item.
pub const MAX_PREVIEW_CHARS: usize = 120;

/// What a project's roadmap says, or why it says nothing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoadmapState {
    /// No roadmap file. Unknown, deliberately not "none".
    Absent,
    /// A roadmap exists but has no checklist items in it — written as prose or
    /// plain bullets. The count is unknown, not zero.
    NoChecklist,
    /// Counted.
    Counted { open: usize, done: usize },
}

/// One project's roadmap, as far as it can be read without running Git.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoadmapSummary {
    pub path: Option<PathBuf>,
    pub state: RoadmapState,
    /// When the roadmap file was last written. `None` when there is no file.
    pub modified: Option<SystemTime>,
    /// The first unchecked item, for a row that would otherwise be a number.
    pub next_item: Option<String>,
}

impl RoadmapSummary {
    fn absent() -> Self {
        Self {
            path: None,
            state: RoadmapState::Absent,
            modified: None,
            next_item: None,
        }
    }

    /// Open items, when the parser could actually count them.
    ///
    /// Deliberately `Option`: a caller that wants to sort by "most work
    /// queued" has to decide what to do about the projects whose count is
    /// unknown, rather than have them silently sort as zero.
    pub fn open_items(&self) -> Option<usize> {
        match self.state {
            RoadmapState::Counted { open, .. } => Some(open),
            _ => None,
        }
    }

    pub fn has_open_work(&self) -> bool {
        matches!(self.state, RoadmapState::Counted { open, .. } if open > 0)
    }
}

/// Read one project's roadmap.
pub fn scan(project: &Path) -> RoadmapSummary {
    let Some(path) = find(project) else {
        return RoadmapSummary::absent();
    };
    let modified = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok();
    let Ok(text) = read_bounded(&path) else {
        return RoadmapSummary {
            path: Some(path),
            state: RoadmapState::NoChecklist,
            modified,
            next_item: None,
        };
    };
    let counted = count(&text);
    RoadmapSummary {
        path: Some(path),
        state: counted.state,
        modified,
        next_item: counted.next_item,
    }
}

fn find(project: &Path) -> Option<PathBuf> {
    ROADMAP_NAMES
        .iter()
        .map(|name| project.join(name))
        .find(|candidate| candidate.is_file())
}

fn read_bounded(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut text = String::new();
    file.take(MAX_ROADMAP_BYTES).read_to_string(&mut text)?;
    Ok(text)
}

/// The result of reading roadmap text, without touching a filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoadmapCount {
    pub state: RoadmapState,
    pub next_item: Option<String>,
}

/// Count checklist items in roadmap text.
///
/// Fenced code blocks are skipped. A roadmap that documents its own format —
/// or quotes another project's — would otherwise have its examples counted as
/// real work, and the number would be wrong in the direction that makes a
/// project look busier than it is.
pub fn count(text: &str) -> RoadmapCount {
    let mut open = 0usize;
    let mut done = 0usize;
    let mut next_item: Option<String> = None;
    let mut fence: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();
        // Fences may be ``` or ~~~ and may be longer than three characters; the
        // closing fence must be at least as long as the opening one.
        if let Some(marker) = fence_marker(trimmed) {
            match &fence {
                Some(open_marker) => {
                    if marker.len() >= open_marker.len()
                        && marker.starts_with(&open_marker[..1])
                    {
                        fence = None;
                    }
                }
                None => fence = Some(marker),
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let Some(item) = checklist_item(trimmed) else {
            continue;
        };
        if item.checked {
            done += 1;
        } else {
            open += 1;
            if next_item.is_none() && !item.text.is_empty() {
                next_item = Some(truncate(item.text));
            }
        }
    }

    let state = if open == 0 && done == 0 {
        // A roadmap with no checkboxes is unreadable to this parser, which is
        // not the same as an empty one.
        RoadmapState::NoChecklist
    } else {
        RoadmapState::Counted { open, done }
    };
    RoadmapCount { state, next_item }
}

fn fence_marker(trimmed: &str) -> Option<String> {
    for character in ['`', '~'] {
        let run: String = trimmed.chars().take_while(|c| *c == character).collect();
        if run.len() >= 3 {
            return Some(run);
        }
    }
    None
}

struct Item<'a> {
    checked: bool,
    text: &'a str,
}

/// A GitHub-flavoured task list item: `- [ ]`, `* [x]`, `+ [X]`.
fn checklist_item(trimmed: &str) -> Option<Item<'_>> {
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    let rest = rest.trim_start();
    if let Some(rest) = rest.strip_prefix("[ ]") {
        return Some(Item {
            checked: false,
            text: rest.trim(),
        });
    }
    rest.strip_prefix("[x]")
        .or_else(|| rest.strip_prefix("[X]"))
        .map(|rest| Item {
            checked: true,
            text: rest.trim(),
        })
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_PREVIEW_CHARS {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_PREVIEW_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counted(text: &str) -> RoadmapCount {
        count(text)
    }

    #[test]
    fn open_and_done_items_are_counted_separately() {
        let result = counted("- [ ] first\n- [x] second\n- [ ] third\n");
        assert_eq!(result.state, RoadmapState::Counted { open: 2, done: 1 });
        assert_eq!(result.next_item.as_deref(), Some("first"));
    }

    #[test]
    fn a_roadmap_with_no_checkboxes_is_unknown_rather_than_empty() {
        // Plenty of projects write a roadmap as prose. Counting zero there
        // claims the work is finished, when the truth is this parser cannot
        // tell — and it would sort beside a genuinely finished project.
        let result = counted("# Roadmap\n\nSome plans, written as prose.\n\n- a plain bullet\n");
        assert_eq!(result.state, RoadmapState::NoChecklist);
        assert_eq!(result.open_items_for_test(), None);
    }

    #[test]
    fn examples_inside_code_fences_are_not_real_work() {
        // A roadmap that documents its own format would otherwise report items
        // that do not exist, in the direction that makes a project look busier.
        let text = "- [ ] real\n\n```markdown\n- [ ] an example\n- [x] another\n```\n\n- [ ] also real\n";
        assert_eq!(counted(text).state, RoadmapState::Counted { open: 2, done: 0 });
    }

    #[test]
    fn a_tilde_fence_and_a_long_fence_both_close_correctly() {
        // An unclosed fence would swallow the rest of the file and report zero.
        let text = "~~~\n- [ ] hidden\n~~~\n- [ ] visible\n";
        assert_eq!(counted(text).state, RoadmapState::Counted { open: 1, done: 0 });
        let longer = "````\n- [ ] hidden\n````\n- [ ] visible\n";
        assert_eq!(counted(longer).state, RoadmapState::Counted { open: 1, done: 0 });
    }

    #[test]
    fn a_fence_opened_with_backticks_is_not_closed_by_tildes() {
        let text = "```\n- [ ] hidden\n~~~\n- [ ] still hidden\n";
        assert_eq!(counted(text).state, RoadmapState::NoChecklist);
    }

    #[test]
    fn indented_and_alternately_bulleted_items_still_count() {
        let text = "  - [ ] indented\n* [ ] star\n+ [x] plus\n";
        assert_eq!(counted(text).state, RoadmapState::Counted { open: 2, done: 1 });
    }

    #[test]
    fn a_capital_x_marks_an_item_done() {
        assert_eq!(counted("- [X] done\n").state, RoadmapState::Counted { open: 0, done: 1 });
    }

    #[test]
    fn something_that_merely_looks_like_a_checkbox_is_not_one() {
        // A prose line mentioning [ ] should not become an item.
        let text = "The syntax is - [ ]\n- not a checkbox\n- [] malformed\n";
        // The first line is not a list item at all (it does not start with a
        // bullet), and the others are not checkboxes.
        assert_eq!(counted(text).state, RoadmapState::NoChecklist);
    }

    #[test]
    fn the_preview_is_the_first_open_item_not_the_first_item() {
        let result = counted("- [x] already done\n- [ ] what is next\n");
        assert_eq!(result.next_item.as_deref(), Some("what is next"));
    }

    #[test]
    fn a_very_long_item_is_truncated_for_a_row() {
        let long = "x".repeat(MAX_PREVIEW_CHARS + 50);
        let result = counted(&format!("- [ ] {long}\n"));
        let preview = result.next_item.expect("preview");
        assert_eq!(preview.chars().count(), MAX_PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn a_project_with_no_roadmap_is_unknown_not_finished() {
        let dir = std::env::temp_dir().join(format!("terminalai-roadmap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let summary = scan(&dir);
        assert_eq!(summary.state, RoadmapState::Absent);
        assert_eq!(summary.open_items(), None);
        assert!(!summary.has_open_work());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanning_reads_the_file_and_records_when_it_changed() {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-roadmap-real-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("ROADMAP.md"), "- [ ] one\n- [x] two\n").expect("write");

        let summary = scan(&dir);
        assert_eq!(summary.state, RoadmapState::Counted { open: 1, done: 1 });
        assert_eq!(summary.open_items(), Some(1));
        assert!(summary.has_open_work());
        assert!(summary.modified.is_some(), "staleness needs a timestamp");
        assert!(summary.path.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    impl RoadmapCount {
        fn open_items_for_test(&self) -> Option<usize> {
            match self.state {
                RoadmapState::Counted { open, .. } => Some(open),
                _ => None,
            }
        }
    }
}
