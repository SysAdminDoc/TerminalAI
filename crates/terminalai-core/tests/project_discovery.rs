//! Discovery against whatever repositories are actually on this machine.
//!
//! The unit tests build directory shapes by hand, which proves the rules but
//! not that they survive a real tree — 100+ repositories with submodules,
//! node_modules, build outputs and linked worktrees in them.

use std::path::PathBuf;
use std::time::Instant;

use terminalai_core::project;

fn repos_root() -> Option<PathBuf> {
    let root = dirs::home_dir()?.join("repos");
    root.is_dir().then_some(root)
}

#[test]
fn discovery_over_a_real_repository_root_is_fast_and_sane() {
    let Some(root) = repos_root() else {
        // Not this machine. The unit tests carry the rules.
        return;
    };
    let started = Instant::now();
    let projects = project::discover(&root);
    let elapsed = started.elapsed();

    assert!(!projects.is_empty(), "no repositories found under {}", root.display());
    // This runs whenever the launcher opens, so it has to be quick enough that
    // nobody waits for it.
    assert!(
        elapsed.as_millis() < 4000,
        "discovery took {elapsed:?} over {} projects",
        projects.len()
    );
    // Every result is a real repository, and none is nested inside another.
    for candidate in &projects {
        assert!(
            project::is_repository(&candidate.path),
            "{} is not a repository",
            candidate.path.display()
        );
        for other in &projects {
            if other.path != candidate.path {
                assert!(
                    !candidate.path.starts_with(&other.path),
                    "{} is inside {}",
                    candidate.path.display(),
                    other.path.display()
                );
            }
        }
    }
    assert!(projects.len() <= project::MAX_PROJECTS);
}
