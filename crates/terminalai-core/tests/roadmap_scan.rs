//! The roadmap scanner against the repositories actually on this machine.
//!
//! The unit tests build roadmap text by hand, which proves the parser. This
//! proves the scan survives a few hundred real files written by different
//! people to no common convention — and that it stays quick enough to run
//! while a dialog is opening.

use std::path::PathBuf;
use std::time::Instant;

use terminalai_core::roadmap::{self, RoadmapState};
use terminalai_core::project;

fn repos_root() -> Option<PathBuf> {
    let root = dirs::home_dir()?.join("repos");
    root.is_dir().then_some(root)
}

#[test]
fn scanning_every_real_project_is_quick_and_never_claims_unknown_work_is_none() {
    let Some(root) = repos_root() else {
        return;
    };
    let projects = project::discover(&root);
    if projects.is_empty() {
        return;
    }

    let started = Instant::now();
    let summaries: Vec<_> = projects
        .iter()
        .map(|candidate| (candidate.name.clone(), roadmap::scan(&candidate.path)))
        .collect();
    let elapsed = started.elapsed();

    assert!(
        elapsed.as_millis() < 5000,
        "scanning {} projects took {elapsed:?}",
        summaries.len()
    );

    for (name, summary) in &summaries {
        match &summary.state {
            // The distinction the whole module exists for: a project with no
            // roadmap must never report a count.
            RoadmapState::Absent => {
                assert!(summary.open_items().is_none(), "{name} counted without a roadmap");
                assert!(summary.path.is_none());
                assert!(!summary.has_open_work());
            }
            RoadmapState::NoChecklist => {
                assert!(summary.open_items().is_none(), "{name} counted an unreadable roadmap");
                assert!(summary.path.is_some(), "{name} has no path for a roadmap it read");
            }
            RoadmapState::Counted { open, .. } => {
                assert!(summary.path.is_some(), "{name} counted with no file");
                assert!(summary.modified.is_some(), "{name} counted with no timestamp");
                assert_eq!(summary.has_open_work(), *open > 0, "{name}");
                if *open > 0 {
                    assert!(summary.next_item.is_some(), "{name} has open items but no preview");
                }
            }
        }
    }

    // This repository is one of them, and it definitely has a checklist.
    if let Some((_, ours)) = summaries.iter().find(|(name, _)| name == "TerminalAI") {
        assert!(
            matches!(ours.state, RoadmapState::Counted { .. }),
            "our own roadmap did not parse: {:?}",
            ours.state
        );
    }
}
