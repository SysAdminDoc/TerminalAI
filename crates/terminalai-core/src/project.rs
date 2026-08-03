//! Every repository under a registered root, as launch targets.
//!
//! The launcher's folder picker asks the operator to browse to a directory they
//! have visited a hundred times. Registering a root once — `~/repos` — turns
//! that into a list, and the list is what the roadmap scanner and the work queue
//! are built on: both need "which projects exist" before they can answer
//! anything about them.
//!
//! Discovery is deliberately shallow and deliberately cheap:
//!
//! - **A repository is a directory containing `.git`**, and the walk stops
//!   there. Descending into one would enumerate every submodule and vendored
//!   dependency as a project of its own, which is how a list of thirty
//!   repositories becomes a list of four hundred.
//! - **No Git process is run.** Branch and dirty state are per-project
//!   questions answered on demand; spawning `git` a hundred times to populate a
//!   dropdown would cost seconds every time the launcher opens.
//! - **Depth is bounded** rather than trusted to terminate. A directory tree on
//!   Windows can contain junctions that loop, and an unbounded walk of a home
//!   directory is not something a UI thread can wait for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How far below a root a repository will be found.
///
/// Two levels covers the shapes people actually use — `~/repos/<name>` and
/// `~/src/<org>/<name>` — without turning a home directory into a full tree
/// walk. A repository nested deeper is registered by adding its own root.
pub const MAX_DEPTH: usize = 2;

/// Cap on projects returned from one root. A dropdown longer than this is not a
/// list anyone reads, and the bound keeps a mistakenly registered root — `C:\`
/// — from stalling the caller.
pub const MAX_PROJECTS: usize = 500;

/// Directory names never descended into. Each is either enormous, or a place a
/// `.git` would not mean what it looks like.
const SKIPPED: [&str; 8] = [
    ".git",
    "node_modules",
    "target",
    "vendor",
    ".venv",
    "__pycache__",
    ".cargo",
    ".rustup",
];

/// One repository found under a registered root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Project {
    /// The directory name. What the operator calls the project.
    pub name: String,
    pub path: PathBuf,
    /// The root it was found under, so a project can be attributed when several
    /// roots are registered.
    pub root: PathBuf,
}

/// Find every repository under `root`.
///
/// A root that does not exist yields nothing rather than an error: roots are
/// persisted, and a drive that is not mounted this session is a normal state,
/// not a configuration problem to report on every launcher open.
pub fn discover(root: &Path) -> Vec<Project> {
    let found = discover_bounded(root);
    if found.len() >= MAX_PROJECTS {
        // Said out loud. A truncated list looks exactly like a complete one,
        // and the operator would conclude the missing repository is not a
        // repository rather than that the cap was reached.
        tracing::warn!(
            root = %root.display(),
            limit = MAX_PROJECTS,
            "project discovery hit its limit; some repositories under this root are not listed"
        );
    }
    found
}

fn discover_bounded(root: &Path) -> Vec<Project> {
    let mut found = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    // A registered root may itself be a repository. Someone who registers one
    // project directly should get that project, not an empty list.
    if is_repository(root) {
        if let Some(project) = project_at(root, root) {
            return vec![project];
        }
    }
    walk(root, root, 0, &mut found, &mut seen);
    found.sort();
    found
}

/// Find every repository under every registered root, de-duplicated.
///
/// Roots overlap in practice — `~/repos` and `~/repos/work` — and a project
/// listed twice is a project the operator has to pick between for no reason.
pub fn discover_all(roots: &[PathBuf]) -> Vec<Project> {
    let mut found: Vec<Project> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for root in roots {
        for project in discover(root) {
            if seen.insert(project.path.clone()) {
                found.push(project);
            }
        }
    }
    found.sort();
    found
}

fn walk(
    root: &Path,
    directory: &Path,
    depth: usize,
    found: &mut Vec<Project>,
    seen: &mut BTreeSet<PathBuf>,
) {
    if depth > MAX_DEPTH || found.len() >= MAX_PROJECTS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if found.len() >= MAX_PROJECTS {
            return;
        }
        let path = entry.path();
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if SKIPPED.contains(&name.as_str()) {
            continue;
        }
        if is_repository(&path) {
            // Found one: do not descend. A repository's submodules and vendored
            // dependencies are part of it, not projects beside it.
            if seen.insert(path.clone()) {
                if let Some(project) = project_at(root, &path) {
                    found.push(project);
                }
            }
            continue;
        }
        walk(root, &path, depth + 1, found, seen);
    }
}

/// True when this directory is the top of a working tree.
///
/// `.git` is a directory in an ordinary clone and a *file* in a worktree or a
/// submodule, so both are accepted — a linked worktree is still a place an
/// agent can work.
pub fn is_repository(path: &Path) -> bool {
    let git = path.join(".git");
    git.is_dir() || git.is_file()
}

fn project_at(root: &Path, path: &Path) -> Option<Project> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())?;
    Some(Project {
        name,
        path: path.to_path_buf(),
        root: root.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-projects-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Scratch(dir)
    }

    /// A directory that looks like a clone, without running git.
    fn repo(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        std::fs::create_dir_all(path.join(".git")).expect("repo");
        path
    }

    fn plain(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        std::fs::create_dir_all(&path).expect("dir");
        path
    }

    #[test]
    fn every_repository_under_a_root_becomes_a_project() {
        let root = scratch("basic");
        repo(&root.0, "shop");
        repo(&root.0, "api");
        plain(&root.0, "notes");

        let projects = discover(&root.0);
        let names: Vec<_> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["api", "shop"], "sorted, and no plain directory");
    }

    #[test]
    fn a_repository_s_own_submodules_are_not_projects_beside_it() {
        // This is how a list of thirty repositories becomes a list of four
        // hundred: every vendored dependency has a .git of its own.
        let root = scratch("nested");
        let shop = repo(&root.0, "shop");
        repo(&shop, "vendored-lib");
        std::fs::create_dir_all(shop.join("crates").join("inner").join(".git")).expect("inner");

        let projects = discover(&root.0);
        assert_eq!(projects.len(), 1, "{projects:?}");
        assert_eq!(projects[0].name, "shop");
    }

    #[test]
    fn a_repository_one_level_further_down_is_still_found() {
        // `~/src/<org>/<name>` is as common as `~/repos/<name>`.
        let root = scratch("org");
        let org = plain(&root.0, "acme");
        repo(&org, "shop");

        let projects = discover(&root.0);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "shop");
    }

    #[test]
    fn the_walk_stops_before_it_becomes_a_tree_walk() {
        // An unbounded walk of a home directory is not something a UI can wait
        // for, and Windows junctions can loop.
        let root = scratch("deep");
        let mut path = root.0.clone();
        for level in 0..(MAX_DEPTH + 3) {
            path = plain(&path, &format!("level{level}"));
        }
        repo(&path, "too-deep");
        assert!(discover(&root.0).is_empty());
    }

    #[test]
    fn registering_a_repository_itself_yields_that_repository() {
        // Someone who registers one project directly should get that project,
        // not an empty list they cannot explain.
        let root = scratch("direct");
        let shop = repo(&root.0, "shop");
        let projects = discover(&shop);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, shop);
    }

    #[test]
    fn a_root_that_is_not_there_yields_nothing_rather_than_failing() {
        // Roots are persisted; an unmounted drive is a normal state, not a
        // configuration problem to report on every launcher open.
        assert!(discover(Path::new(r"Z:\not\mounted")).is_empty());
    }

    #[test]
    fn overlapping_roots_do_not_list_a_project_twice() {
        // `~/repos` and `~/repos/work` are both reasonable to register.
        let root = scratch("overlap");
        let work = plain(&root.0, "work");
        repo(&work, "shop");

        let projects = discover_all(&[root.0.clone(), work.clone()]);
        assert_eq!(projects.len(), 1, "{projects:?}");
    }

    #[test]
    fn heavy_directories_are_never_descended_into() {
        // node_modules alone can hold thousands of directories, and packages
        // inside it carry .git often enough to matter.
        let root = scratch("heavy");
        let shop = plain(&root.0, "shop");
        repo(&shop.join("node_modules"), "some-package");
        std::fs::create_dir_all(shop.join("node_modules")).expect("node_modules");

        assert!(discover(&root.0).is_empty());
    }

    #[test]
    fn a_linked_worktree_counts_as_a_place_to_work() {
        // `.git` is a file rather than a directory in a worktree or submodule
        // checkout, and an agent can work in one perfectly well.
        let root = scratch("worktree");
        let linked = plain(&root.0, "shop-feature");
        std::fs::write(linked.join(".git"), "gitdir: ../shop/.git/worktrees/feature")
            .expect("gitdir file");

        let projects = discover(&root.0);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "shop-feature");
    }

    #[test]
    fn a_project_records_which_root_it_came_from() {
        let root = scratch("attribution");
        repo(&root.0, "shop");
        assert_eq!(discover(&root.0)[0].root, root.0);
    }
}
