//! Saved and built-in launch configurations.
//!
//! Three kinds of thing end up in the launcher's preset list, and they answer
//! different questions:
//!
//! - **Built-ins** ship with the app. The store starts empty, and an empty
//!   dropdown on first run makes the operator invent a configuration before
//!   they have any idea which ones matter. These are defined in code, never
//!   written to disk, and never editable in place — an operator who edits one
//!   and later gets a corrected version in an update would silently keep the
//!   old one. Cloning under a new name is how you change a built-in.
//! - **Saved presets** are the operator's own, and behave as they always have.
//! - **Hidden built-ins** are the only state a built-in contributes to the
//!   file: a list of names to stop offering. Hiding rather than deleting means
//!   the choice is reversible, which matters because there is no other way to
//!   get a built-in back.
//!
//! A built-in carries no working directory. Launch configuration and *which
//! project* are separate choices, and a preset that also moved the folder would
//! make picking "Plan first" quietly retarget the session.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use terminalai_core::atomic_file::write_atomic;
use terminalai_core::launch::{Effort, LaunchSpec, Permission, Sandbox};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub spec: LaunchSpec,
    pub configured_path: Option<PathBuf>,
    /// True for a preset that ships with the app. Serialized so the UI can mark
    /// it and refuse to overwrite it; never persisted, because built-ins are
    /// not stored.
    #[serde(default)]
    pub builtin: bool,
    /// Why this configuration exists, for a dropdown that would otherwise be
    /// four words of jargon.
    #[serde(default)]
    pub description: Option<String>,
}

/// The on-disk shape.
///
/// The first version of this file was a bare JSON array of presets. That form is
/// still read — an operator's saved presets must survive an update that adds
/// built-ins — and rewritten in the current shape on the next save.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredPresets {
    #[serde(default)]
    presets: Vec<Preset>,
    /// Names of built-ins the operator has hidden.
    #[serde(default)]
    hidden_builtins: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StoredPresetsFile {
    Current(StoredPresets),
    /// Pre-0.6.0: a bare array.
    Legacy(Vec<Preset>),
}

impl From<StoredPresetsFile> for StoredPresets {
    fn from(file: StoredPresetsFile) -> Self {
        match file {
            StoredPresetsFile::Current(current) => current,
            StoredPresetsFile::Legacy(presets) => StoredPresets {
                presets,
                hidden_builtins: BTreeSet::new(),
            },
        }
    }
}

#[derive(Clone)]
pub struct PresetStore {
    path: PathBuf,
    state: Arc<Mutex<StoredPresets>>,
}

/// The configurations offered on a fresh install.
///
/// Chosen to span the axes an operator actually decides between — which agent,
/// and how much rope it gets — rather than to enumerate every combination. A
/// menu nobody reads is the same as an empty one.
///
/// The bypass entry is deliberately paired with worktree isolation. An agent
/// that never asks permission belongs in its own checkout, and shipping the
/// dangerous half of that pair without the safe half would be the app
/// recommending it. Its description says so out loud, because on native Windows
/// neither agent has a first-party filesystem sandbox — Claude Code's is macOS,
/// Linux and WSL2 only — so the worktree and the environment lease are not a
/// belt-and-braces addition to a sandbox, they are the whole of the isolation.
fn builtins() -> Vec<Preset> {
    fn preset(
        name: &str,
        description: &str,
        spec: LaunchSpec,
    ) -> Preset {
        Preset {
            name: name.to_owned(),
            spec,
            configured_path: None,
            builtin: true,
            description: Some(description.to_owned()),
        }
    }
    use terminalai_core::agent::Agent;
    vec![
        preset(
            "Claude · Plan first",
            "Reads and proposes, makes no edits",
            LaunchSpec {
                agent: Agent::Claude,
                permission: Some(Permission::Plan),
                effort: Some(Effort::High),
                ..LaunchSpec::default()
            },
        ),
        preset(
            "Claude · Build",
            "Edits files, still asks before running commands",
            LaunchSpec {
                agent: Agent::Claude,
                permission: Some(Permission::AcceptEdits),
                effort: Some(Effort::High),
                ..LaunchSpec::default()
            },
        ),
        preset(
            "Claude · Full auto, isolated",
            "Never asks, and Windows offers it no sandbox — so its own worktree is the isolation",
            LaunchSpec {
                agent: Agent::Claude,
                permission: Some(Permission::Bypass),
                effort: Some(Effort::High),
                worktree: true,
                ..LaunchSpec::default()
            },
        ),
        preset(
            "Claude · Quick question",
            "Low effort, no edits — for a fast answer",
            LaunchSpec {
                agent: Agent::Claude,
                permission: Some(Permission::Plan),
                effort: Some(Effort::Low),
                ..LaunchSpec::default()
            },
        ),
        preset(
            "Codex · Plan first",
            "Read-only sandbox, proposes without touching the tree",
            LaunchSpec {
                agent: Agent::Codex,
                permission: Some(Permission::Plan),
                sandbox: Some(Sandbox::ReadOnly),
                effort: Some(Effort::High),
                ..LaunchSpec::default()
            },
        ),
        preset(
            "Codex · Build",
            "Writes inside the workspace, network off",
            LaunchSpec {
                agent: Agent::Codex,
                permission: Some(Permission::AcceptEdits),
                sandbox: Some(Sandbox::WorkspaceWrite),
                effort: Some(Effort::High),
                ..LaunchSpec::default()
            },
        ),
    ]
}

/// Whether a name belongs to a built-in.
pub fn is_builtin_name(name: &str) -> bool {
    builtins().iter().any(|preset| preset.name == name)
}

impl PresetStore {
    pub fn load_default() -> Result<Self, String> {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| {
                "could not determine the local application-data directory".to_string()
            })?;
        Self::load_from(base.join("TerminalAI").join("presets.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self, String> {
        let state = if path.is_file() {
            let contents =
                fs::read_to_string(&path).map_err(|error| format!("read presets: {error}"))?;
            let file: StoredPresetsFile = serde_json::from_str(&contents)
                .map_err(|error| format!("parse presets: {error}"))?;
            StoredPresets::from(file)
        } else {
            StoredPresets::default()
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// Everything the launcher should offer: built-ins that are not hidden,
    /// then the operator's own.
    ///
    /// Built-ins come first because on a fresh install they are the only thing
    /// there, and because a list that puts them last buries the answer to
    /// "what do I pick" under the operator's accumulated one-offs.
    pub fn list(&self) -> Result<Vec<Preset>, String> {
        let state = self.lock()?;
        let mut out: Vec<Preset> = builtins()
            .into_iter()
            .filter(|preset| !state.hidden_builtins.contains(&preset.name))
            .collect();
        out.extend(state.presets.iter().cloned());
        Ok(out)
    }

    /// Saved presets only, without built-ins — what is actually on disk.
    #[cfg(test)]
    pub fn saved(&self) -> Result<Vec<Preset>, String> {
        self.lock().map(|state| state.presets.clone())
    }

    pub fn save(&self, mut preset: Preset) -> Result<(), String> {
        preset.name = preset.name.trim().to_string();
        validate_name(&preset.name)?;
        // Refused rather than shadowed. A saved preset with a built-in's name
        // would look like an edited built-in while being a separate thing, so a
        // corrected built-in in a later version would silently never appear.
        if is_builtin_name(&preset.name) {
            return Err(format!(
                "“{}” is a built-in preset and cannot be edited. Save it under a different name to \
                 make your own copy.",
                preset.name
            ));
        }
        // A saved preset is the operator's, whatever the caller claims.
        preset.builtin = false;
        let mut state = self.lock()?;
        if let Some(existing) = state
            .presets
            .iter_mut()
            .find(|entry| entry.name == preset.name)
        {
            *existing = preset;
        } else {
            state.presets.push(preset);
        }
        state.presets.sort_by_key(|entry| entry.name.to_lowercase());
        self.persist(&state)
    }

    /// Remove a saved preset, or hide a built-in.
    ///
    /// Hiding rather than deleting because a built-in cannot be recreated by
    /// hand — the name would then collide with the built-in it replaced.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut state = self.lock()?;
        if is_builtin_name(name) {
            return Ok(state.hidden_builtins.insert(name.to_owned()) && {
                self.persist(&state)?;
                true
            });
        }
        let before = state.presets.len();
        state.presets.retain(|entry| entry.name != name);
        if state.presets.len() == before {
            return Ok(false);
        }
        self.persist(&state)?;
        Ok(true)
    }

    /// Offer every built-in again.
    ///
    /// Hiding is otherwise a one-way door: there is no other route back to a
    /// preset that only exists in code.
    pub fn restore_builtins(&self) -> Result<usize, String> {
        let mut state = self.lock()?;
        let restored = state.hidden_builtins.len();
        if restored == 0 {
            return Ok(0);
        }
        state.hidden_builtins.clear();
        self.persist(&state)?;
        Ok(restored)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, StoredPresets>, String> {
        self.state
            .lock()
            .map_err(|_| "preset store lock is poisoned".to_string())
    }

    fn persist(&self, state: &StoredPresets) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "preset path has no parent".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("create preset directory: {error}"))?;
        let json =
            serde_json::to_vec_pretty(state).map_err(|error| format!("encode presets: {error}"))?;
        write_atomic(&self.path, &json, true).map_err(|error| format!("write presets: {error}"))
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("preset name cannot be empty".into());
    }
    if name.chars().count() > 80 {
        return Err("preset name cannot exceed 80 characters".into());
    }
    if name.chars().any(|character| character.is_control()) {
        return Err("preset name cannot contain control characters".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use terminalai_core::agent::Agent;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn store() -> (PresetStore, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "terminalai-presets-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        (PresetStore::load_from(path.clone()).expect("store"), path)
    }

    fn spec() -> LaunchSpec {
        LaunchSpec {
            agent: Agent::Codex,
            cwd: Path::new(".").to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn saves_updates_and_sorts_named_presets() {
        let (store, path) = store();
        store
            .save(Preset {
                name: "zeta".into(),
                spec: spec(),
                configured_path: None,
                builtin: false,
                description: None,
            })
            .expect("save");
        store
            .save(Preset {
                name: "Alpha".into(),
                spec: spec(),
                configured_path: None,
                builtin: false,
                description: None,
            })
            .expect("save");
        assert_eq!(store.saved().expect("saved")[0].name, "Alpha");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_empty_names() {
        assert!(validate_name(" ").is_ok(), "trim happens before validation");
        assert!(validate_name("").is_err());
    }

    #[test]
    fn a_fresh_install_offers_something_to_pick() {
        // An empty dropdown makes the operator invent a configuration before
        // they know which ones matter.
        let (store, path) = store();
        let listed = store.list().expect("list");
        assert!(listed.len() >= 4, "only {} presets on a fresh install", listed.len());
        assert!(listed.iter().all(|preset| preset.builtin));
        assert!(
            listed.iter().all(|preset| preset.description.is_some()),
            "a built-in with no description is four words of jargon"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_built_in_never_carries_a_working_directory() {
        // Which configuration and which project are separate choices; a preset
        // that moved the folder would quietly retarget the session.
        for preset in builtins() {
            assert_eq!(
                preset.spec.cwd,
                PathBuf::new(),
                "{} sets a working directory",
                preset.name
            );
        }
    }

    #[test]
    fn the_never_asks_preset_is_the_isolated_one() {
        // Shipping the dangerous half of that pair without the safe half would
        // be the app recommending it.
        let bypass: Vec<_> = builtins()
            .into_iter()
            .filter(|preset| preset.spec.permission == Some(Permission::Bypass))
            .collect();
        assert!(!bypass.is_empty(), "the axis is not covered at all");
        for preset in bypass {
            assert!(preset.spec.worktree, "{} bypasses without isolation", preset.name);
            // An operator reading "never asks" cannot tell a missing sandbox
            // flag from a missing sandbox. On native Windows it is the latter,
            // and the worktree is the only thing standing in for one.
            let description = preset
                .description
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                description.contains("sandbox") && description.contains("worktree"),
                "{} does not say what is isolating it: {description:?}",
                preset.name
            );
        }
    }

    #[test]
    fn a_built_in_cannot_be_edited_in_place() {
        // An operator who edited one would keep their copy forever, including
        // after a later version corrected the original.
        let (store, path) = store();
        let name = builtins()[0].name.clone();
        let error = store
            .save(Preset {
                name: name.clone(),
                spec: spec(),
                configured_path: None,
                builtin: false,
                description: None,
            })
            .expect_err("must refuse");
        assert!(error.contains("built-in"), "{error}");
        assert!(error.contains("different name"), "no way forward: {error}");
        assert!(store.saved().expect("saved").is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_built_in_can_be_cloned_under_another_name() {
        let (store, path) = store();
        let original = builtins()[0].clone();
        store
            .save(Preset {
                name: "My plan mode".into(),
                spec: original.spec.clone(),
                configured_path: None,
                builtin: true,
                description: None,
            })
            .expect("clone");
        let saved = store.saved().expect("saved");
        assert_eq!(saved.len(), 1);
        // The claim of being built-in does not survive: it is the operator's.
        assert!(!saved[0].builtin);
        assert_eq!(saved[0].spec.permission, original.spec.permission);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn hiding_a_built_in_removes_it_from_the_list_and_is_reversible() {
        // There is no other way back to a preset that only exists in code.
        let (store, path) = store();
        let name = builtins()[0].name.clone();
        let before = store.list().expect("list").len();
        assert!(store.delete(&name).expect("hide"));
        let after = store.list().expect("list");
        assert_eq!(after.len(), before - 1);
        assert!(!after.iter().any(|preset| preset.name == name));

        assert_eq!(store.restore_builtins().expect("restore"), 1);
        assert_eq!(store.list().expect("list").len(), before);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_hidden_built_in_stays_hidden_across_a_restart() {
        let (store, path) = store();
        let name = builtins()[0].name.clone();
        store.delete(&name).expect("hide");
        drop(store);

        let reopened = PresetStore::load_from(path.clone()).expect("reopen");
        assert!(!reopened
            .list()
            .expect("list")
            .iter()
            .any(|preset| preset.name == name));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn presets_saved_before_built_ins_existed_still_load() {
        // The first version of this file was a bare JSON array. An operator's
        // saved presets must survive the update that added built-ins.
        let path = std::env::temp_dir().join(format!(
            "terminalai-presets-legacy-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let legacy = serde_json::to_string(&vec![Preset {
            name: "mine".into(),
            spec: spec(),
            configured_path: None,
            builtin: false,
            description: None,
        }])
        .expect("encode");
        fs::write(&path, legacy).expect("write");

        let store = PresetStore::load_from(path.clone()).expect("load");
        let saved = store.saved().expect("saved");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "mine");
        // And the built-ins are offered alongside it.
        assert!(store.list().expect("list").len() > 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn hiding_something_that_is_not_a_preset_at_all_reports_nothing_removed() {
        let (store, path) = store();
        assert!(!store.delete("no such preset").expect("delete"));
        let _ = fs::remove_file(path);
    }
}
