use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use terminalai_core::launch::LaunchSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub spec: LaunchSpec,
    pub configured_path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct PresetStore {
    path: PathBuf,
    entries: Arc<Mutex<Vec<Preset>>>,
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
        let entries = if path.is_file() {
            let contents =
                fs::read_to_string(&path).map_err(|error| format!("read presets: {error}"))?;
            serde_json::from_str(&contents).map_err(|error| format!("parse presets: {error}"))?
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            entries: Arc::new(Mutex::new(entries)),
        })
    }

    pub fn list(&self) -> Result<Vec<Preset>, String> {
        self.entries
            .lock()
            .map_err(|_| "preset store lock is poisoned".to_string())
            .map(|entries| entries.clone())
    }

    pub fn save(&self, mut preset: Preset) -> Result<(), String> {
        preset.name = preset.name.trim().to_string();
        validate_name(&preset.name)?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "preset store lock is poisoned".to_string())?;
        if let Some(existing) = entries.iter_mut().find(|entry| entry.name == preset.name) {
            *existing = preset;
        } else {
            entries.push(preset);
        }
        entries.sort_by_key(|entry| entry.name.to_lowercase());
        self.persist(&entries)
    }

    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "preset store lock is poisoned".to_string())?;
        let before = entries.len();
        entries.retain(|entry| entry.name != name);
        if entries.len() == before {
            return Ok(false);
        }
        self.persist(&entries)?;
        Ok(true)
    }

    fn persist(&self, entries: &[Preset]) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "preset path has no parent".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("create preset directory: {error}"))?;
        let json = serde_json::to_vec_pretty(entries)
            .map_err(|error| format!("encode presets: {error}"))?;
        let temp = self
            .path
            .with_extension(format!("json.tmp-{}", std::process::id()));
        fs::write(&temp, json).map_err(|error| format!("write presets: {error}"))?;
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|error| format!("replace presets: {error}"))?;
        }
        fs::rename(&temp, &self.path).map_err(|error| format!("commit presets: {error}"))
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
    use terminalai_core::agent::Agent;

    #[test]
    fn saves_updates_and_sorts_named_presets() {
        let path =
            std::env::temp_dir().join(format!("terminalai-presets-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let store = PresetStore::load_from(path.clone()).expect("store");
        let spec = LaunchSpec {
            agent: Agent::Codex,
            cwd: Path::new(".").to_path_buf(),
            ..Default::default()
        };
        store
            .save(Preset {
                name: "zeta".into(),
                spec: spec.clone(),
                configured_path: None,
            })
            .expect("save");
        store
            .save(Preset {
                name: "Alpha".into(),
                spec,
                configured_path: None,
            })
            .expect("save");
        assert_eq!(store.list().expect("list")[0].name, "Alpha");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_empty_names() {
        assert!(validate_name(" ").is_ok(), "trim happens before validation");
        assert!(validate_name("").is_err());
    }
}
