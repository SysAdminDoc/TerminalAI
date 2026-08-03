//! The repository's own template file, parsed as the launcher parses it.
//!
//! This repo ships a `.terminalai/templates.toml`, which makes it the one place
//! the format is exercised against a real file rather than a string literal —
//! and it means a change to the format that breaks the file is caught here
//! rather than by an operator opening the launcher.

use std::path::Path;

use terminalai_core::launch::{Effort, LaunchSpec, Permission};
use terminalai_core::template;

fn repo_root() -> &'static Path {
    // The crate directory is two levels below the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
}

#[test]
fn this_repository_s_own_templates_load_and_apply() {
    let templates = template::load(repo_root()).expect("this repo's templates must parse");
    assert!(!templates.is_empty(), "the shipped template file is empty");

    let drain = templates
        .iter()
        .find(|item| item.name.contains("roadmap"))
        .expect("a roadmap template");
    let mut spec = LaunchSpec::default();
    drain.apply(repo_root(), &mut spec);
    assert_eq!(spec.cwd, repo_root());
    assert_eq!(spec.effort, Some(Effort::High));
    assert_eq!(spec.permission, Some(Permission::AcceptEdits));
    assert!(spec.worktree, "the drain template is meant to be isolated");
}

#[test]
fn every_shipped_template_is_usable_without_further_choices() {
    // A template that names no agent leaves the launcher on whatever was last
    // selected, which is the opposite of what a project-declared template is
    // for.
    for item in template::load(repo_root()).expect("templates") {
        assert!(item.agent.is_some(), "{:?} names no agent", item.name);
        assert!(
            item.description.is_some(),
            "{:?} has no description, so the dropdown says only its name",
            item.name
        );
    }
}
