//! Named fleet layouts: many sessions saved and relaunched as one action.
//!
//! A preset configures *one* session. Rebuilding a twelve-session spread after
//! a restart meant twelve trips through the launcher, which is the most-asked-
//! for missing feature in the two largest competitors.
//!
//! # Restoring is not a second launch path
//!
//! The one design constraint that matters. Every member is started through the
//! same `Request::Launch` the launcher uses, one at a time, so admission, the
//! memory budget, the spend ceiling and the dirty-tree refusal all apply
//! without this module knowing they exist. A restore that assembled sessions
//! itself would be a way to bypass every limit the fleet has — and it would
//! bypass the *next* one too, silently, on the day it is added.
//!
//! A refusal is therefore an expected outcome, not an error: the restore
//! reports per member what happened and keeps going, because eleven of twelve
//! sessions is a useful result and a transaction that rolled back the other
//! eleven is not.
//!
//! # What is not saved
//!
//! No worktree path and no branch. `LaunchSpec::worktree` is a request for a
//! private checkout, and restoring it creates a fresh one — it never adopts the
//! checkout the original session had, for the same reason the worktree feature
//! already refuses to: two sessions sharing one checkout is the failure the
//! feature exists to prevent, and a saved layout is the easiest way to arrange
//! it by accident.
//!
//! No session output, no resume id, no cost. A layout is what to start, not
//! what happened.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use terminalai_core::atomic_file::write_atomic;
use terminalai_core::launch::LaunchSpec;

/// Most sessions one saved layout may hold.
///
/// Well past the fleet's ~30-session design point. Bounded so a corrupted or
/// hand-edited file cannot ask the daemon for an unbounded number of launches.
pub const MAX_MEMBERS: usize = 64;
/// Most layouts kept. The list is a menu; past this it is a filing problem.
pub const MAX_WORKING_SETS: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSetMember {
    pub spec: LaunchSpec,
    /// The project folder as the operator configured it, kept separate from
    /// `spec.cwd` exactly as the launcher keeps it.
    #[serde(default)]
    pub configured_path: Option<PathBuf>,
    /// Whether this session held a live terminal grid. Restored on a best
    /// effort: the fleet refuses a fourth pin, and a layout saved with more is
    /// reported rather than silently trimmed.
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSet {
    pub name: String,
    pub members: Vec<WorkingSetMember>,
    /// The fleet's grouping mode when the layout was captured.
    ///
    /// A view setting rather than session data — grouping is derived from
    /// fields the sessions already have — but restoring the spread without the
    /// arrangement the operator was reading it in gets half the job done.
    #[serde(default)]
    pub group_by: Option<String>,
}

/// What happened to one member of a restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreOutcome {
    pub name: String,
    pub cwd: PathBuf,
    /// The new session's id, when one was created.
    pub id: Option<String>,
    /// True when the fleet admitted it as queued rather than starting it. Not a
    /// failure: the queue is the admission gate doing its job.
    #[serde(default)]
    pub queued: bool,
    /// Why this member did not start, when it did not.
    #[serde(default)]
    pub refused: Option<String>,
    /// Why the pin was not restored, when it was asked for and declined.
    #[serde(default)]
    pub pin_refused: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredWorkingSets {
    #[serde(default)]
    sets: Vec<WorkingSet>,
}

#[derive(Clone)]
pub struct WorkingSetStore {
    path: PathBuf,
    state: Arc<Mutex<StoredWorkingSets>>,
}

impl WorkingSetStore {
    pub fn load_default() -> Result<Self, String> {
        let base = dirs::data_local_dir().or_else(dirs::data_dir).ok_or_else(|| {
            "could not determine the local application-data directory".to_string()
        })?;
        Self::load_from(base.join("TerminalAI").join("working-sets.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self, String> {
        let stored = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<StoredWorkingSets>(&text)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                StoredWorkingSets::default()
            }
            Err(error) => return Err(format!("could not read {}: {error}", path.display())),
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(stored)),
        })
    }

    pub fn list(&self) -> Vec<WorkingSet> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sets
            .clone()
    }

    /// Save a layout, replacing one of the same name.
    ///
    /// Replacing rather than refusing: re-saving under the name you already use
    /// is how a layout gets updated, and an operator who meant a new one types
    /// a new name.
    pub fn save(&self, mut set: WorkingSet) -> Result<(), String> {
        set.name = set.name.trim().to_owned();
        if set.name.is_empty() {
            return Err("a working set needs a name".into());
        }
        if set.members.is_empty() {
            // Saving an empty fleet would produce a layout that restores
            // nothing, which looks exactly like a broken restore later.
            return Err("there are no sessions to save".into());
        }
        if set.members.len() > MAX_MEMBERS {
            return Err(format!("a working set holds at most {MAX_MEMBERS} sessions"));
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match state.sets.iter_mut().find(|existing| existing.name == set.name) {
                Some(existing) => *existing = set,
                None => {
                    if state.sets.len() >= MAX_WORKING_SETS {
                        return Err(format!("at most {MAX_WORKING_SETS} working sets are kept"));
                    }
                    state.sets.push(set);
                }
            }
        }
        self.persist()
    }

    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let removed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let before = state.sets.len();
            state.sets.retain(|set| set.name != name);
            state.sets.len() != before
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    pub fn get(&self, name: &str) -> Option<WorkingSet> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sets
            .iter()
            .find(|set| set.name == name)
            .cloned()
    }

    fn persist(&self) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let text = serde_json::to_string_pretty(&*state)
            .map_err(|error| format!("could not encode working sets: {error}"))?;
        // Keep a backup, as the preset store does: a layout is minutes of
        // configuration and there is no other copy of it.
        write_atomic(&self.path, text.as_bytes(), true)
            .map_err(|error| format!("could not write {}: {error}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use terminalai_core::agent::Agent;
    use terminalai_core::launch::spec_for;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "terminalai-working-sets-{}-{name}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_file(&path);
        Scratch(path)
    }

    fn member(pinned: bool) -> WorkingSetMember {
        WorkingSetMember {
            spec: spec_for(Agent::Claude, std::path::Path::new(".")),
            configured_path: None,
            pinned,
        }
    }

    fn a_set(name: &str, members: usize) -> WorkingSet {
        WorkingSet {
            name: name.into(),
            members: (0..members).map(|_| member(false)).collect(),
            group_by: Some("folder".into()),
        }
    }

    #[test]
    fn a_layout_survives_a_reload() {
        let file = scratch("reload");
        let store = WorkingSetStore::load_from(file.0.clone()).expect("store");
        store.save(a_set("morning", 3)).expect("save");

        let reloaded = WorkingSetStore::load_from(file.0.clone()).expect("reload");
        let sets = reloaded.list();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].members.len(), 3);
        assert_eq!(sets[0].group_by.as_deref(), Some("folder"));
    }

    #[test]
    fn saving_the_same_name_updates_rather_than_duplicating() {
        // Re-saving under the name already in use is how a layout is updated.
        let file = scratch("replace");
        let store = WorkingSetStore::load_from(file.0.clone()).expect("store");
        store.save(a_set("morning", 3)).expect("save");
        store.save(a_set("morning", 5)).expect("resave");
        let sets = store.list();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].members.len(), 5);
    }

    #[test]
    fn an_empty_fleet_cannot_be_saved_as_a_layout() {
        // A layout that restores nothing is indistinguishable from a restore
        // that failed, at the moment the operator is relying on it.
        let file = scratch("empty");
        let store = WorkingSetStore::load_from(file.0.clone()).expect("store");
        assert!(store.save(a_set("empty", 0)).is_err());
        assert!(store.list().is_empty());
    }

    #[test]
    fn a_nameless_layout_is_refused() {
        let file = scratch("nameless");
        let store = WorkingSetStore::load_from(file.0.clone()).expect("store");
        let mut set = a_set("   ", 1);
        set.name = "   ".into();
        assert!(store.save(set).is_err());
    }

    #[test]
    fn the_member_count_is_bounded() {
        // A hand-edited file must not be able to ask the daemon for an
        // unbounded number of launches.
        let file = scratch("bounded");
        let store = WorkingSetStore::load_from(file.0.clone()).expect("store");
        assert!(store.save(a_set("huge", MAX_MEMBERS + 1)).is_err());
        assert!(store.save(a_set("exact", MAX_MEMBERS)).is_ok());
    }

    #[test]
    fn deleting_reports_whether_anything_was_there() {
        let file = scratch("delete");
        let store = WorkingSetStore::load_from(file.0.clone()).expect("store");
        store.save(a_set("morning", 1)).expect("save");
        assert!(store.delete("morning").expect("delete"));
        assert!(!store.delete("morning").expect("delete again"));
        assert!(store.list().is_empty());
    }

    #[test]
    fn a_missing_file_is_an_empty_list_rather_than_an_error() {
        // First run. An error here would make the menu unopenable.
        let file = scratch("absent");
        let store = WorkingSetStore::load_from(file.0.clone()).expect("store");
        assert!(store.list().is_empty());
    }
}
