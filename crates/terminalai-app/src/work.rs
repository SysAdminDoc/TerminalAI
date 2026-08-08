//! The stored prompt library and the run that applies one across projects.
//!
//! The prompts this is built for are the operator's real ones — several
//! kilobytes of prose each. Two consequences shape the whole module:
//!
//! - **Storage holds text, not commands.** A prompt is never interpolated into
//!   anything that gets parsed.
//! - **Delivery is a pty write.** A run launches a session with no initial
//!   prompt and then puts the text on that session's prompt queue, which writes
//!   it as a bracketed paste. Passing it as an argument would hit the command
//!   line, where Windows quoting mangles `&`, `^`, `|` and `%` — the same
//!   reason the launcher resolves native binaries rather than going through
//!   `cmd.exe`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use terminalai_core::atomic_file::write_atomic;
use terminalai_core::schedule::WorkSchedule;
use terminalai_core::work_queue::WorkQueue;

/// Cap on stored prompts.
pub const MAX_PROMPTS: usize = 64;
/// Cap on one stored prompt. The operator's drain template is about 11 KB, so
/// this is generous while still bounding the file.
pub const MAX_PROMPT_BYTES: usize = 128 * 1024;

/// One reusable instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPrompt {
    pub name: String,
    pub text: String,
    /// Where it was seeded from, when it was seeded rather than typed.
    #[serde(default)]
    pub source: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredLibrary {
    #[serde(default)]
    prompts: Vec<StoredPrompt>,
    /// Set once the first-run seed has been attempted, so a prompt the operator
    /// deletes is not silently restored on the next launch.
    #[serde(default)]
    seeded: bool,
}

#[derive(Clone)]
pub struct PromptLibrary {
    path: PathBuf,
    state: Arc<Mutex<StoredLibrary>>,
}

/// Prompts seeded on first run, when the operator's own copies are present.
///
/// Read from disk rather than embedded: these are the operator's templates, and
/// a copy compiled into the binary would drift from the file they actually
/// maintain. Nothing is invented — if the file is not there, no prompt is
/// created, because a stored prompt named "drain the roadmap" that contains
/// something this app made up would be worse than an empty library.
fn seed_sources(home: &Path) -> Vec<(&'static str, PathBuf)> {
    let prompts = home.join(".claude").join("prompts");
    vec![
        ("Research new roadmap items", prompts.join("research-deep.txt")),
        ("Drain the roadmap", prompts.join("roadmap-drain.txt")),
    ]
}

impl PromptLibrary {
    pub fn load_default() -> Result<Self, String> {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| "could not determine the local application-data directory".to_string())?;
        let library = Self::load_from(base.join("TerminalAI").join("prompts.json"))?;
        if let Some(home) = dirs::home_dir() {
            library.seed_once(&home)?;
        }
        Ok(library)
    }

    pub fn load_from(path: PathBuf) -> Result<Self, String> {
        let state = if path.is_file() {
            let contents =
                fs::read_to_string(&path).map_err(|error| format!("read prompts: {error}"))?;
            serde_json::from_str(&contents).map_err(|error| format!("parse prompts: {error}"))?
        } else {
            StoredLibrary::default()
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// Copy the operator's own prompt templates in, once.
    pub fn seed_once(&self, home: &Path) -> Result<usize, String> {
        let mut state = self.lock()?;
        if state.seeded {
            return Ok(0);
        }
        state.seeded = true;
        let mut added = 0;
        for (name, source) in seed_sources(home) {
            if state.prompts.iter().any(|prompt| prompt.name == name) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&source) else {
                continue;
            };
            let text = text.trim().to_owned();
            if text.is_empty() || text.len() > MAX_PROMPT_BYTES {
                continue;
            }
            state.prompts.push(StoredPrompt {
                name: name.to_owned(),
                text,
                source: Some(source),
            });
            added += 1;
        }
        self.persist(&state)?;
        Ok(added)
    }

    pub fn list(&self) -> Result<Vec<StoredPrompt>, String> {
        self.lock().map(|state| state.prompts.clone())
    }

    pub fn get(&self, name: &str) -> Result<Option<StoredPrompt>, String> {
        Ok(self
            .lock()?
            .prompts
            .iter()
            .find(|prompt| prompt.name == name)
            .cloned())
    }

    pub fn save(&self, mut prompt: StoredPrompt) -> Result<(), String> {
        prompt.name = prompt.name.trim().to_owned();
        if prompt.name.is_empty() {
            return Err("a stored prompt needs a name".into());
        }
        if prompt.text.trim().is_empty() {
            return Err("a stored prompt cannot be empty".into());
        }
        if prompt.text.len() > MAX_PROMPT_BYTES {
            return Err(format!(
                "a stored prompt cannot exceed {MAX_PROMPT_BYTES} bytes"
            ));
        }
        let mut state = self.lock()?;
        if let Some(existing) = state
            .prompts
            .iter_mut()
            .find(|existing| existing.name == prompt.name)
        {
            *existing = prompt;
        } else {
            if state.prompts.len() >= MAX_PROMPTS {
                return Err(format!("at most {MAX_PROMPTS} prompts can be stored"));
            }
            state.prompts.push(prompt);
        }
        state.prompts.sort_by_key(|prompt| prompt.name.to_lowercase());
        self.persist(&state)
    }

    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut state = self.lock()?;
        let before = state.prompts.len();
        state.prompts.retain(|prompt| prompt.name != name);
        if state.prompts.len() == before {
            return Ok(false);
        }
        self.persist(&state)?;
        Ok(true)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, StoredLibrary>, String> {
        self.state
            .lock()
            .map_err(|_| "prompt library lock is poisoned".to_string())
    }

    fn persist(&self, state: &StoredLibrary) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "prompt library path has no parent".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("create prompt directory: {error}"))?;
        let json =
            serde_json::to_vec_pretty(state).map_err(|error| format!("encode prompts: {error}"))?;
        write_atomic(&self.path, &json, true).map_err(|error| format!("write prompts: {error}"))
    }
}

/// The current run, persisted so it survives a restart.
#[derive(Clone)]
pub struct WorkRunStore {
    path: PathBuf,
    run: Arc<Mutex<Option<WorkQueue>>>,
}

impl WorkRunStore {
    pub fn load_default() -> Result<Self, String> {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| "could not determine the local application-data directory".to_string())?;
        Self::load_from(base.join("TerminalAI").join("work-run.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self, String> {
        let run = if path.is_file() {
            let contents =
                fs::read_to_string(&path).map_err(|error| format!("read work run: {error}"))?;
            serde_json::from_str(&contents).map_err(|error| format!("parse work run: {error}"))?
        } else {
            None
        };
        Ok(Self {
            path,
            run: Arc::new(Mutex::new(run)),
        })
    }

    pub fn get(&self) -> Result<Option<WorkQueue>, String> {
        self.lock().map(|run| run.clone())
    }

    pub fn set(&self, queue: Option<WorkQueue>) -> Result<(), String> {
        let mut run = self.lock()?;
        *run = queue;
        self.persist(&run)
    }

    /// Change the stored run in place.
    pub fn update<T>(&self, edit: impl FnOnce(&mut WorkQueue) -> T) -> Result<Option<T>, String> {
        let mut run = self.lock()?;
        let Some(queue) = run.as_mut() else {
            return Ok(None);
        };
        let result = edit(queue);
        self.persist(&run)?;
        Ok(Some(result))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<WorkQueue>>, String> {
        self.run
            .lock()
            .map_err(|_| "work run lock is poisoned".to_string())
    }

    fn persist(&self, run: &Option<WorkQueue>) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "work run path has no parent".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("create work run directory: {error}"))?;
        let json =
            serde_json::to_vec_pretty(run).map_err(|error| format!("encode work run: {error}"))?;
        write_atomic(&self.path, &json, true).map_err(|error| format!("write work run: {error}"))
    }
}

/// The standing schedule, if the operator set one.
///
/// Its own file rather than a field on the run: the run is replaced every time
/// one starts, and a schedule that vanished with the run it started would only
/// ever fire once. Persisted for the same reason the run is — a schedule that
/// did not survive a restart would be a promise this tool cannot keep.
#[derive(Clone)]
pub struct WorkScheduleStore {
    path: PathBuf,
    schedule: Arc<Mutex<Option<WorkSchedule>>>,
}

impl WorkScheduleStore {
    pub fn load_default() -> Result<Self, String> {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| "could not determine the local application-data directory".to_string())?;
        Self::load_from(base.join("TerminalAI").join("work-schedule.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self, String> {
        let schedule = if path.is_file() {
            let contents =
                fs::read_to_string(&path).map_err(|error| format!("read work schedule: {error}"))?;
            // A schedule that cannot be parsed is dropped rather than fatal: it
            // is a convenience, and refusing to start the window over one would
            // be worse than losing it.
            serde_json::from_str(&contents).unwrap_or_default()
        } else {
            None
        };
        Ok(Self {
            path,
            schedule: Arc::new(Mutex::new(schedule)),
        })
    }

    pub fn get(&self) -> Result<Option<WorkSchedule>, String> {
        self.lock().map(|schedule| schedule.clone())
    }

    pub fn set(&self, schedule: Option<WorkSchedule>) -> Result<(), String> {
        let mut held = self.lock()?;
        *held = schedule;
        self.persist(&held)
    }

    /// Change the stored schedule in place, persisting whatever the edit left.
    pub fn update<T>(&self, edit: impl FnOnce(&mut WorkSchedule) -> T) -> Result<Option<T>, String> {
        let mut held = self.lock()?;
        let Some(schedule) = held.as_mut() else {
            return Ok(None);
        };
        let result = edit(schedule);
        self.persist(&held)?;
        Ok(Some(result))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<WorkSchedule>>, String> {
        self.schedule
            .lock()
            .map_err(|_| "work schedule lock is poisoned".to_string())
    }

    fn persist(&self, schedule: &Option<WorkSchedule>) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "work schedule path has no parent".to_string())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create work schedule directory: {error}"))?;
        let json = serde_json::to_vec_pretty(schedule)
            .map_err(|error| format!("encode work schedule: {error}"))?;
        write_atomic(&self.path, &json, true)
            .map_err(|error| format!("write work schedule: {error}"))
    }
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
            "terminalai-work-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        Scratch(dir)
    }

    fn open_library(dir: &Path) -> PromptLibrary {
        PromptLibrary::load_from(dir.join("prompts.json")).expect("library")
    }

    /// A home directory holding the operator's own prompt templates.
    fn home_with_templates(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        let prompts = dir.join(".claude").join("prompts");
        fs::create_dir_all(&prompts).expect("prompts dir");
        for (name, text) in files {
            fs::write(prompts.join(name), text).expect("template");
        }
        dir.to_path_buf()
    }

    #[test]
    fn the_library_seeds_from_the_operators_own_templates() {
        let dir = scratch();
        let home = home_with_templates(
            &dir.0,
            &[
                ("research-deep.txt", "survey prior art thoroughly"),
                ("roadmap-drain.txt", "implement the next roadmap item"),
            ],
        );
        let library = open_library(&dir.0);
        assert_eq!(library.seed_once(&home).expect("seed"), 2);
        let prompts = library.list().expect("list");
        assert_eq!(prompts.len(), 2);
        assert!(prompts.iter().any(|p| p.text.contains("survey prior art")));
        assert!(prompts.iter().all(|p| p.source.is_some()));
    }

    #[test]
    fn nothing_is_invented_when_a_template_is_missing() {
        // A stored prompt named "drain the roadmap" containing something this
        // app made up would be worse than an empty library.
        let dir = scratch();
        let home = home_with_templates(&dir.0, &[("roadmap-drain.txt", "only this one")]);
        let library = open_library(&dir.0);
        assert_eq!(library.seed_once(&home).expect("seed"), 1);
        assert_eq!(library.list().expect("list").len(), 1);
    }

    #[test]
    fn a_deleted_seeded_prompt_does_not_come_back_on_the_next_launch() {
        let dir = scratch();
        let home = home_with_templates(&dir.0, &[("roadmap-drain.txt", "drain it")]);
        let library = open_library(&dir.0);
        library.seed_once(&home).expect("seed");
        assert!(library.delete("Drain the roadmap").expect("delete"));

        // A fresh handle on the same file, as a restart would produce.
        let reopened = open_library(&dir.0);
        assert_eq!(reopened.seed_once(&home).expect("seed again"), 0);
        assert!(reopened.list().expect("list").is_empty());
    }

    #[test]
    fn a_multi_kilobyte_prompt_round_trips_intact() {
        // The whole point: these are 6–11 KB of prose, and anything that
        // truncated or re-quoted them would be silently wrong.
        let dir = scratch();
        let library = open_library(&dir.0);
        let text = format!(
            "{}\n\nSpecial characters that a command line would mangle: & ^ | % \" ' `\n",
            "line of prose ".repeat(600)
        );
        library
            .save(StoredPrompt {
                name: "big".into(),
                text: text.clone(),
                source: None,
            })
            .expect("save");
        let reopened = open_library(&dir.0);
        assert_eq!(reopened.get("big").expect("get").expect("prompt").text, text);
    }

    #[test]
    fn an_empty_or_oversized_prompt_is_refused() {
        let dir = scratch();
        let library = open_library(&dir.0);
        assert!(library
            .save(StoredPrompt {
                name: "x".into(),
                text: "  ".into(),
                source: None
            })
            .is_err());
        assert!(library
            .save(StoredPrompt {
                name: "".into(),
                text: "text".into(),
                source: None
            })
            .is_err());
        assert!(library
            .save(StoredPrompt {
                name: "big".into(),
                text: "x".repeat(MAX_PROMPT_BYTES + 1),
                source: None
            })
            .is_err());
    }

    #[test]
    fn seeding_reads_the_operators_real_templates_when_they_are_present() {
        // The synthetic seed test proves the mechanism; this proves it against
        // the actual multi-kilobyte files the feature was designed around, so a
        // path or encoding mistake surfaces here rather than on first run.
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let present: Vec<_> = seed_sources(&home)
            .into_iter()
            .filter(|(_, path)| path.is_file())
            .collect();
        if present.is_empty() {
            // Not this machine.
            return;
        }
        let dir = scratch();
        let library = open_library(&dir.0);
        let added = library.seed_once(&home).expect("seed");
        assert_eq!(added, present.len(), "not every present template was seeded");
        for prompt in library.list().expect("list") {
            assert!(
                prompt.text.len() > 1000,
                "{} seeded only {} bytes",
                prompt.name,
                prompt.text.len()
            );
            assert!(prompt.source.is_some());
            // Read as text, so nothing was truncated at a byte boundary.
            assert!(!prompt.text.contains(char::REPLACEMENT_CHARACTER), "{}", prompt.name);
        }
    }

    #[test]
    fn a_run_survives_a_restart() {
        use terminalai_core::work_queue::WorkQueue;
        let dir = scratch();
        let store = WorkRunStore::load_from(dir.0.join("work-run.json")).expect("store");
        let queue = WorkQueue::new(
            "Drain the roadmap",
            &[("shop".into(), PathBuf::from(r"C:\repos\shop"))],
        )
        .expect("queue");
        store.set(Some(queue.clone())).expect("set");

        let reopened = WorkRunStore::load_from(dir.0.join("work-run.json")).expect("reopen");
        assert_eq!(reopened.get().expect("get"), Some(queue));
    }

    #[test]
    fn updating_a_run_that_does_not_exist_reports_nothing_rather_than_creating_one() {
        let dir = scratch();
        let store = WorkRunStore::load_from(dir.0.join("work-run.json")).expect("store");
        assert_eq!(store.update(|queue| queue.entries.len()).expect("update"), None);
    }

    fn a_schedule() -> WorkSchedule {
        WorkSchedule::new(
            "Drain the roadmap",
            vec![PathBuf::from("/repos/shop")],
            std::time::Duration::from_secs(4 * 3600),
            std::time::SystemTime::now(),
        )
        .expect("schedule")
    }

    #[test]
    fn a_schedule_survives_a_restart_with_its_record_intact() {
        // The whole promise: a schedule that did not outlive the window is not
        // a schedule, and its history is the only account of what ran while the
        // operator was away.
        let dir = scratch();
        let path = dir.0.join("work-schedule.json");
        let store = WorkScheduleStore::load_from(path.clone()).expect("store");
        let mut schedule = a_schedule();
        schedule.record(terminalai_core::schedule::ScheduleFiring {
            at: std::time::SystemTime::now(),
            result: terminalai_core::schedule::FiringResult::Started { projects: 3 },
            missed: 2,
        });
        store.set(Some(schedule.clone())).expect("set");

        let reopened = WorkScheduleStore::load_from(path).expect("reopen");
        assert_eq!(reopened.get().expect("get"), Some(schedule));
    }

    #[test]
    fn a_schedule_file_that_cannot_be_read_is_dropped_rather_than_fatal() {
        // It is a convenience. Refusing to open the window over one would cost
        // the operator the whole fleet for the sake of a repeating run.
        let dir = scratch();
        let path = dir.0.join("work-schedule.json");
        fs::write(&path, b"{ not json at all").expect("write");
        let store = WorkScheduleStore::load_from(path).expect("store");
        assert_eq!(store.get().expect("get"), None);
    }

    #[test]
    fn updating_a_schedule_that_does_not_exist_creates_nothing() {
        let dir = scratch();
        let store =
            WorkScheduleStore::load_from(dir.0.join("work-schedule.json")).expect("store");
        assert_eq!(store.update(|schedule| schedule.paused = true).expect("update"), None);
        assert_eq!(store.get().expect("get"), None);
    }
}
