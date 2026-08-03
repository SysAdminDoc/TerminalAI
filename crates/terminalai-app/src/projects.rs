//! The roots the operator has registered, and the projects under them.
//!
//! Only the roots are stored. The projects themselves are re-discovered rather
//! than cached, because the list's whole value is being current: a repository
//! cloned five minutes ago should be launchable without telling the app about
//! it, and one deleted last week should not still be offered. A cache would
//! need invalidation nobody would remember to trigger.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use terminalai_core::atomic_file::write_atomic;
use terminalai_core::project::{self, Project};
use terminalai_core::roadmap::{self, RoadmapSummary};

/// Cap on registered roots. Each one is walked on every refresh, so this is
/// what keeps a refresh bounded regardless of what has been registered.
pub const MAX_ROOTS: usize = 16;

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredRoots {
    #[serde(default)]
    roots: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct ProjectRoots {
    path: PathBuf,
    roots: Arc<Mutex<Vec<PathBuf>>>,
}

impl ProjectRoots {
    pub fn load_default() -> Result<Self, String> {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| "could not determine the local application-data directory".to_string())?;
        Self::load_from(base.join("TerminalAI").join("projects.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self, String> {
        let roots = if path.is_file() {
            let contents =
                fs::read_to_string(&path).map_err(|error| format!("read project roots: {error}"))?;
            let stored: StoredRoots = serde_json::from_str(&contents)
                .map_err(|error| format!("parse project roots: {error}"))?;
            stored.roots
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            roots: Arc::new(Mutex::new(roots)),
        })
    }

    pub fn list(&self) -> Result<Vec<PathBuf>, String> {
        self.lock().map(|roots| roots.clone())
    }

    /// Register a root.
    ///
    /// A root already covered by a registered one is refused rather than
    /// silently accepted: it would add nothing, and the operator would be left
    /// wondering why removing it changed no projects.
    pub fn add(&self, root: PathBuf) -> Result<(), String> {
        if !root.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }
        let mut roots = self.lock()?;
        if roots.len() >= MAX_ROOTS {
            return Err(format!(
                "at most {MAX_ROOTS} project roots can be registered; remove one first"
            ));
        }
        if roots.iter().any(|existing| existing == &root) {
            return Err(format!("{} is already registered", root.display()));
        }
        if let Some(covering) = roots.iter().find(|existing| covers(existing, &root)) {
            return Err(format!(
                "{} is already covered by {}",
                root.display(),
                covering.display()
            ));
        }
        // A new root that covers existing ones replaces them, so registering
        // `~/repos` after `~/repos/work` leaves one root rather than two that
        // find the same projects.
        roots.retain(|existing| !covers(&root, existing));
        roots.push(root);
        roots.sort();
        self.persist(&roots)
    }

    pub fn remove(&self, root: &Path) -> Result<bool, String> {
        let mut roots = self.lock()?;
        let before = roots.len();
        roots.retain(|existing| existing != root);
        if roots.len() == before {
            return Ok(false);
        }
        self.persist(&roots)?;
        Ok(true)
    }

    /// Every project under every registered root, discovered fresh.
    pub fn projects(&self) -> Result<Vec<Project>, String> {
        let roots = self.list()?;
        Ok(project::discover_all(&roots))
    }

    /// Every project with what its roadmap says.
    ///
    /// One file read per project rather than one Git process per project: this
    /// runs across a few hundred repositories at once, and a `git log` each
    /// would make the answer cost more than it is worth.
    pub fn scanned(&self) -> Result<Vec<ScannedProject>, String> {
        Ok(self
            .projects()?
            .into_iter()
            .map(|project| {
                let roadmap = roadmap::scan(&project.path);
                ScannedProject { project, roadmap }
            })
            .collect())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Vec<PathBuf>>, String> {
        self.roots
            .lock()
            .map_err(|_| "project root store lock is poisoned".to_string())
    }

    fn persist(&self, roots: &[PathBuf]) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "project root path has no parent".to_string())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create project root directory: {error}"))?;
        let json = serde_json::to_vec_pretty(&StoredRoots {
            roots: roots.to_vec(),
        })
        .map_err(|error| format!("encode project roots: {error}"))?;
        write_atomic(&self.path, &json, true)
            .map_err(|error| format!("write project roots: {error}"))
    }
}

/// A project and what its roadmap says.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedProject {
    #[serde(flatten)]
    pub project: Project,
    pub roadmap: RoadmapSummary,
}

/// True when `parent` contains `child`, or is it.
fn covers(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-roots-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        Scratch(dir)
    }

    fn store(dir: &Path) -> ProjectRoots {
        ProjectRoots::load_from(dir.join("projects.json")).expect("store")
    }

    fn repo(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        fs::create_dir_all(path.join(".git")).expect("repo");
        path
    }

    #[test]
    fn a_registered_root_makes_its_repositories_launchable() {
        let dir = scratch();
        let root = dir.0.join("repos");
        fs::create_dir_all(&root).expect("root");
        repo(&root, "shop");
        repo(&root, "api");

        let store = store(&dir.0);
        store.add(root).expect("add");
        let names: Vec<_> = store
            .projects()
            .expect("projects")
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["api", "shop"]);
    }

    #[test]
    fn projects_are_rediscovered_rather_than_cached() {
        // The list's whole value is being current: a repository cloned five
        // minutes ago should be launchable without telling the app about it.
        let dir = scratch();
        let root = dir.0.join("repos");
        fs::create_dir_all(&root).expect("root");
        let store = store(&dir.0);
        store.add(root.clone()).expect("add");
        assert!(store.projects().expect("projects").is_empty());

        repo(&root, "cloned-just-now");
        assert_eq!(store.projects().expect("projects").len(), 1);

        fs::remove_dir_all(root.join("cloned-just-now")).expect("remove");
        assert!(store.projects().expect("projects").is_empty(), "a deleted repo is still offered");
    }

    #[test]
    fn a_root_already_covered_by_another_is_refused() {
        // It would add nothing, and removing it later would change no projects,
        // which is not something the operator could explain.
        let dir = scratch();
        let outer = dir.0.join("repos");
        let inner = outer.join("work");
        fs::create_dir_all(&inner).expect("dirs");
        let store = store(&dir.0);
        store.add(outer.clone()).expect("add outer");
        let error = store.add(inner).expect_err("must refuse");
        assert!(error.contains("already covered"), "{error}");
        assert_eq!(store.list().expect("list").len(), 1);
    }

    #[test]
    fn a_broader_root_replaces_the_ones_it_covers() {
        let dir = scratch();
        let outer = dir.0.join("repos");
        let inner = outer.join("work");
        fs::create_dir_all(&inner).expect("dirs");
        let store = store(&dir.0);
        store.add(inner).expect("add inner");
        store.add(outer.clone()).expect("add outer");
        assert_eq!(store.list().expect("list"), vec![outer]);
    }

    #[test]
    fn registering_the_same_root_twice_is_refused_by_name() {
        let dir = scratch();
        let root = dir.0.join("repos");
        fs::create_dir_all(&root).expect("root");
        let store = store(&dir.0);
        store.add(root.clone()).expect("add");
        let error = store.add(root).expect_err("refuse");
        assert!(error.contains("already registered"), "{error}");
    }

    #[test]
    fn a_root_that_is_not_a_directory_is_refused_at_registration() {
        // Refused when it can be explained, rather than silently yielding no
        // projects every time the launcher opens.
        let dir = scratch();
        let file = dir.0.join("not-a-directory");
        fs::write(&file, b"x").expect("file");
        let store = store(&dir.0);
        assert!(store.add(file).is_err());
    }

    #[test]
    fn roots_survive_a_restart() {
        let dir = scratch();
        let root = dir.0.join("repos");
        fs::create_dir_all(&root).expect("root");
        store(&dir.0).add(root.clone()).expect("add");
        assert_eq!(store(&dir.0).list().expect("list"), vec![root]);
    }

    #[test]
    fn removing_a_root_stops_offering_its_projects() {
        let dir = scratch();
        let root = dir.0.join("repos");
        fs::create_dir_all(&root).expect("root");
        repo(&root, "shop");
        let store = store(&dir.0);
        store.add(root.clone()).expect("add");
        assert!(store.remove(&root).expect("remove"));
        assert!(store.projects().expect("projects").is_empty());
        assert!(!store.remove(&root).expect("remove again"));
    }

    #[test]
    fn scanning_reports_what_each_project_still_has_queued() {
        use terminalai_core::roadmap::RoadmapState;
        let dir = scratch();
        let root = dir.0.join("repos");
        fs::create_dir_all(&root).expect("root");
        let busy = repo(&root, "busy");
        fs::write(busy.join("ROADMAP.md"), "- [ ] one
- [ ] two
- [x] done
").expect("write");
        let quiet = repo(&root, "quiet");
        fs::write(quiet.join("ROADMAP.md"), "- [x] all done
").expect("write");
        repo(&root, "unknown");

        let store = store(&dir.0);
        store.add(root).expect("add");
        let scanned = store.scanned().expect("scan");
        assert_eq!(scanned.len(), 3);

        let by_name = |name: &str| {
            scanned
                .iter()
                .find(|item| item.project.name == name)
                .unwrap_or_else(|| panic!("{name} missing"))
                .clone()
        };
        assert_eq!(by_name("busy").roadmap.open_items(), Some(2));
        assert!(by_name("busy").roadmap.has_open_work());
        assert_eq!(by_name("quiet").roadmap.open_items(), Some(0));
        assert!(!by_name("quiet").roadmap.has_open_work());
        // A project with no roadmap is unknown, and must not read as finished.
        assert_eq!(by_name("unknown").roadmap.state, RoadmapState::Absent);
        assert_eq!(by_name("unknown").roadmap.open_items(), None);
    }

    #[test]
    fn the_number_of_roots_is_bounded() {
        // Every root is walked on every refresh; this is what keeps a refresh
        // bounded no matter what has been registered.
        let dir = scratch();
        let store = store(&dir.0);
        for index in 0..MAX_ROOTS {
            let root = dir.0.join(format!("root{index}"));
            fs::create_dir_all(&root).expect("root");
            store.add(root).expect("add");
        }
        let extra = dir.0.join("one-too-many");
        fs::create_dir_all(&extra).expect("root");
        assert!(store.add(extra).is_err());
    }
}
