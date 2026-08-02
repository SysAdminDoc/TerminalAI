//! Explicit, reversible installation of the agent hook adapters.
//!
//! The writer only owns entries carrying [`MANAGED_MARKER`]. Existing user
//! hooks and unrelated Codex notification commands are retained verbatim.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value as JsonValue};
use toml_edit::{value, Array, ArrayOfTables, Document, Item, Table, Value as TomlValue};

use crate::agent::Agent;

pub const MANAGED_MARKER: &str = "--terminalai-managed";

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "Stop",
    "Notification",
    "PreToolUse",
    "PostToolUse",
];
const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];

#[derive(Debug, thiserror::Error)]
pub enum HookConfigError {
    #[error("hook configuration I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Claude hook settings are invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex hook settings are invalid TOML: {0}")]
    Toml(String),
    #[error("{agent} hook settings have an invalid {field} shape")]
    InvalidShape {
        agent: &'static str,
        field: &'static str,
    },
    #[error("Codex hooks are disabled by the existing features.hooks=false setting")]
    HooksDisabled,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HookChange {
    pub path: PathBuf,
    pub changed: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HookStatus {
    pub agent: Agent,
    pub path: PathBuf,
    pub installed: bool,
    pub stale: bool,
    pub disabled: bool,
    pub fallback_installed: bool,
}

pub fn config_path(agent: Agent, home: &Path, codex_home: Option<&Path>) -> PathBuf {
    match agent {
        Agent::Claude => home.join(".claude").join("settings.json"),
        Agent::Codex => codex_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".codex"))
            .join("config.toml"),
    }
}

pub fn command_for(agent: Agent, executable: &Path) -> String {
    format!(
        "{} hook {} {}",
        shell_quote(executable),
        agent.command_name(),
        MANAGED_MARKER
    )
}

pub fn preview(agent: Agent, executable: &Path) -> String {
    let command = command_for(agent, executable);
    match agent {
        Agent::Claude => {
            let hooks: Map<String, JsonValue> = CLAUDE_EVENTS
                .iter()
                .map(|event| ((*event).into(), json!([claude_group(&command)])))
                .collect();
            serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap_or_default()
        }
        Agent::Codex => {
            let mut doc = Document::new();
            let _ = install_codex_document(&mut doc, &command);
            doc.to_string()
        }
    }
}

pub fn install_at(
    agent: Agent,
    path: &Path,
    executable: &Path,
) -> Result<HookChange, HookConfigError> {
    let before = read_optional(path)?;
    let command = command_for(agent, executable);
    let after = match agent {
        Agent::Claude => install_claude_text(before.as_deref(), &command)?,
        Agent::Codex => install_codex_text(before.as_deref(), &command)?,
    };
    let changed = write_if_changed(path, before.as_deref(), &after)?;
    Ok(HookChange {
        path: path.to_path_buf(),
        changed,
    })
}

pub fn remove_at(
    agent: Agent,
    path: &Path,
    _executable: &Path,
) -> Result<HookChange, HookConfigError> {
    let Some(before) = read_optional(path)? else {
        return Ok(HookChange {
            path: path.to_path_buf(),
            changed: false,
        });
    };
    let after = match agent {
        Agent::Claude => remove_claude_text(&before)?,
        Agent::Codex => remove_codex_text(&before)?,
    };
    let changed = write_if_changed(path, Some(&before), &after)?;
    Ok(HookChange {
        path: path.to_path_buf(),
        changed,
    })
}

pub fn status_at(
    agent: Agent,
    path: &Path,
    executable: &Path,
) -> Result<HookStatus, HookConfigError> {
    let command = command_for(agent, executable);
    let (installed, stale, disabled, fallback_installed) = match read_optional(path)? {
        None => (false, false, false, false),
        Some(text) => match agent {
            Agent::Claude => inspect_claude_text(&text, &command)?,
            Agent::Codex => inspect_codex_text(&text, &command)?,
        },
    };
    Ok(HookStatus {
        agent,
        path: path.to_path_buf(),
        installed: installed || fallback_installed,
        stale,
        disabled,
        fallback_installed,
    })
}

fn install_claude_text(text: Option<&str>, command: &str) -> Result<String, HookConfigError> {
    let mut root = match text {
        Some(text) => serde_json::from_str::<JsonValue>(text)?,
        None => json!({}),
    };
    let object = root.as_object_mut().ok_or(HookConfigError::InvalidShape {
        agent: "Claude",
        field: "root",
    })?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(HookConfigError::InvalidShape {
            agent: "Claude",
            field: "hooks",
        })?;
    for event in CLAUDE_EVENTS {
        let groups = hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or(HookConfigError::InvalidShape {
                agent: "Claude",
                field: "hooks event",
            })?;
        remove_claude_groups(groups, Agent::Claude);
        groups.push(claude_group(command));
    }
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

fn remove_claude_text(text: &str) -> Result<String, HookConfigError> {
    let mut root: JsonValue = serde_json::from_str(text)?;
    let Some(hooks) = root.get_mut("hooks").and_then(JsonValue::as_object_mut) else {
        return Ok(text.to_owned());
    };
    for event in CLAUDE_EVENTS {
        let Some(groups) = hooks.get_mut(*event).and_then(JsonValue::as_array_mut) else {
            continue;
        };
        remove_claude_groups(groups, Agent::Claude);
        if groups.is_empty() {
            hooks.remove(*event);
        }
    }
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

fn remove_claude_groups(groups: &mut Vec<JsonValue>, agent: Agent) -> bool {
    let before = groups.len();
    groups.retain_mut(|group| {
        let Some(group_object) = group.as_object_mut() else {
            return true;
        };
        let Some(handlers) = group_object
            .get_mut("hooks")
            .and_then(JsonValue::as_array_mut)
        else {
            return true;
        };
        handlers.retain(|handler| {
            !handler
                .get("command")
                .and_then(JsonValue::as_str)
                .is_some_and(|command| is_managed_command(command, agent))
        });
        !handlers.is_empty()
    });
    before != groups.len()
}

fn claude_group(command: &str) -> JsonValue {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": command,
            "async": true
        }]
    })
}

fn install_codex_text(text: Option<&str>, command: &str) -> Result<String, HookConfigError> {
    let mut doc = parse_codex(text)?;
    if codex_hooks_disabled(&doc) {
        return Err(HookConfigError::HooksDisabled);
    }
    install_codex_document(&mut doc, command);
    Ok(doc.to_string())
}

fn remove_codex_text(text: &str) -> Result<String, HookConfigError> {
    let mut doc = parse_codex(Some(text))?;
    remove_codex_document(&mut doc, Agent::Codex);
    Ok(doc.to_string())
}

fn install_codex_document(doc: &mut Document, command: &str) -> bool {
    {
        let hooks = doc["hooks"].or_insert(Item::Table(Table::new()));
        let Some(hooks) = hooks.as_table_mut() else {
            return false;
        };
        for event in CODEX_EVENTS {
            let groups = hooks
                .entry(event)
                .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
            let Some(groups) = groups.as_array_of_tables_mut() else {
                continue;
            };
            remove_codex_groups(groups, Agent::Codex);
            groups.push(codex_group(command));
        }
    }
    let notify = doc
        .get("notify")
        .and_then(Item::as_value)
        .and_then(|value| value.as_array());
    if notify.is_none() || is_managed_notify(notify, Agent::Codex) {
        doc.insert(
            "notify",
            Item::Value(TomlValue::Array(notify_values(command))),
        );
    }
    true
}

fn remove_codex_document(doc: &mut Document, agent: Agent) -> bool {
    let mut changed = false;
    if let Some(hooks) = doc.get_mut("hooks").and_then(Item::as_table_mut) {
        let mut remove_events = Vec::new();
        for event in CODEX_EVENTS {
            let Some(groups) = hooks.get_mut(event).and_then(Item::as_array_of_tables_mut) else {
                continue;
            };
            let event_changed = remove_codex_groups(groups, agent);
            changed |= event_changed;
            if groups.is_empty() && event_changed {
                remove_events.push(*event);
            }
        }
        for event in remove_events {
            hooks.remove(event);
        }
    }
    if doc
        .get("notify")
        .and_then(Item::as_value)
        .and_then(|value| value.as_array())
        .is_some_and(|notify| is_managed_notify(Some(notify), agent))
    {
        doc.remove("notify");
        changed = true;
    }
    changed
}

fn remove_codex_groups(groups: &mut ArrayOfTables, agent: Agent) -> bool {
    let mut changed = false;
    groups.retain(|group| {
        let Some(handlers) = group.get("hooks").and_then(Item::as_array_of_tables) else {
            return true;
        };
        let mut kept = 0;
        let mut managed = 0;
        for handler in handlers {
            if handler
                .get("command")
                .and_then(Item::as_value)
                .and_then(|value| value.as_str())
                .is_some_and(|command| is_managed_command(command, agent))
            {
                managed += 1;
            } else {
                kept += 1;
            }
        }
        if managed == 0 {
            return true;
        }
        changed = true;
        if kept == 0 {
            return false;
        }
        true
    });

    // Rebuild groups containing both user and TerminalAI handlers so only the
    // owned handlers disappear. The retain pass above intentionally removes
    // whole groups only when they contained no unrelated handlers.
    for group in groups.iter_mut() {
        if let Some(handlers) = group
            .get_mut("hooks")
            .and_then(Item::as_array_of_tables_mut)
        {
            handlers.retain(|handler| {
                !handler
                    .get("command")
                    .and_then(Item::as_value)
                    .and_then(|value| value.as_str())
                    .is_some_and(|command| is_managed_command(command, agent))
            });
        }
    }
    changed
}

fn codex_group(command: &str) -> Table {
    let mut group = Table::new();
    group["matcher"] = value("");
    let mut handlers = ArrayOfTables::new();
    let mut handler = Table::new();
    handler["type"] = value("command");
    handler["command"] = value(command);
    handlers.push(handler);
    group["hooks"] = Item::ArrayOfTables(handlers);
    group
}

fn parse_codex(text: Option<&str>) -> Result<Document, HookConfigError> {
    match text {
        Some(text) => text
            .parse::<Document>()
            .map_err(|error| HookConfigError::Toml(error.to_string())),
        None => Ok(Document::new()),
    }
}

fn codex_hooks_disabled(doc: &Document) -> bool {
    doc.get("features")
        .and_then(Item::as_table)
        .and_then(|features| features.get("hooks"))
        .and_then(Item::as_value)
        .and_then(|value| value.as_bool())
        == Some(false)
}

fn inspect_claude_text(
    text: &str,
    command: &str,
) -> Result<(bool, bool, bool, bool), HookConfigError> {
    let root: JsonValue = serde_json::from_str(text)?;
    let mut current = false;
    let mut stale = false;
    if let Some(hooks) = root.get("hooks").and_then(JsonValue::as_object) {
        for event in CLAUDE_EVENTS {
            if let Some(groups) = hooks.get(*event).and_then(JsonValue::as_array) {
                for group in groups {
                    if let Some(handlers) = group.get("hooks").and_then(JsonValue::as_array) {
                        for handler in handlers {
                            if let Some(value) = handler.get("command").and_then(JsonValue::as_str)
                            {
                                if is_managed_command(value, Agent::Claude) {
                                    if value == command {
                                        current = true;
                                    } else {
                                        stale = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok((current, stale, false, false))
}

fn inspect_codex_text(
    text: &str,
    command: &str,
) -> Result<(bool, bool, bool, bool), HookConfigError> {
    let doc = parse_codex(Some(text))?;
    let disabled = codex_hooks_disabled(&doc);
    let mut current = false;
    let mut stale = false;
    if let Some(hooks) = doc.get("hooks").and_then(Item::as_table) {
        for event in CODEX_EVENTS {
            if let Some(groups) = hooks.get(event).and_then(Item::as_array_of_tables) {
                for group in groups {
                    if let Some(handlers) = group.get("hooks").and_then(Item::as_array_of_tables) {
                        for handler in handlers {
                            if let Some(value) = handler
                                .get("command")
                                .and_then(Item::as_value)
                                .and_then(|value| value.as_str())
                            {
                                if is_managed_command(value, Agent::Codex) {
                                    if value == command {
                                        current = true;
                                    } else {
                                        stale = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let fallback = doc
        .get("notify")
        .and_then(Item::as_value)
        .and_then(|value| value.as_array())
        .is_some_and(|notify| is_managed_notify(Some(notify), Agent::Codex));
    Ok((current || fallback, stale, disabled, fallback))
}

fn notify_values(command: &str) -> Array {
    let mut notify = Array::new();
    notify.push(command_executable(command));
    notify.push("hook");
    notify.push("codex");
    notify.push(MANAGED_MARKER);
    notify
}

fn command_executable(command: &str) -> &str {
    let command = command.trim();
    match command.as_bytes().first().copied() {
        Some(b'"') => command[1..]
            .split_once('"')
            .map(|(path, _)| path)
            .unwrap_or(&command[1..]),
        Some(b'\'') => command[1..]
            .split_once('\'')
            .map(|(path, _)| path)
            .unwrap_or(&command[1..]),
        _ => command.split_whitespace().next().unwrap_or(command),
    }
}

fn is_managed_notify(notify: Option<&Array>, agent: Agent) -> bool {
    let Some(notify) = notify else { return false };
    let values: Vec<_> = notify.iter().filter_map(|value| value.as_str()).collect();
    values.len() >= 4
        && values[1] == "hook"
        && values[2] == agent.command_name()
        && values[3] == MANAGED_MARKER
}

fn is_managed_command(command: &str, agent: Agent) -> bool {
    command.contains(MANAGED_MARKER)
        && command.contains(&format!(" hook {} ", agent.command_name()))
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    if cfg!(windows) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, HookConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_if_changed(
    path: &Path,
    before: Option<&str>,
    after: &str,
) -> Result<bool, HookConfigError> {
    if before == Some(after) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, after)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-hook-config-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    #[test]
    fn claude_install_is_async_and_preserves_user_hooks() {
        let dir = test_dir();
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"permissions":{"allow":["Read"]},"hooks":{"Notification":[{"matcher":"Bash","hooks":[{"type":"command","command":"user-hook"}]}]}}"#,
        )
        .expect("seed settings");
        let executable = Path::new(r"C:\Program Files\TerminalAI\terminalai.exe");
        install_at(Agent::Claude, &path, executable).expect("install");
        let first = fs::read_to_string(&path).expect("read settings");
        assert!(first.contains(MANAGED_MARKER));
        assert!(first.contains("\"async\": true"));
        assert!(first.contains("user-hook"));
        install_at(Agent::Claude, &path, executable).expect("idempotent install");
        assert_eq!(first, fs::read_to_string(&path).expect("read again"));
        remove_at(Agent::Claude, &path, executable).expect("remove");
        let removed = fs::read_to_string(&path).expect("read removed");
        assert!(!removed.contains(MANAGED_MARKER));
        assert!(removed.contains("user-hook"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn codex_install_uses_hooks_and_notify_without_overwriting_user_notify() {
        let dir = test_dir();
        let path = dir.join("config.toml");
        fs::write(&path, "model = \"gpt-5\"\nnotify = [\"user-notify\"]\n").expect("seed config");
        let executable = Path::new(r"C:\Program Files\TerminalAI\terminalai.exe");
        install_at(Agent::Codex, &path, executable).expect("install");
        let first = fs::read_to_string(&path).expect("read config");
        assert!(first.contains(MANAGED_MARKER));
        assert!(first.contains("[[hooks.PermissionRequest]]"));
        assert!(first.contains("user-notify"));
        install_at(Agent::Codex, &path, executable).expect("idempotent install");
        assert_eq!(first, fs::read_to_string(&path).expect("read again"));
        remove_at(Agent::Codex, &path, executable).expect("remove");
        let removed = fs::read_to_string(&path).expect("read removed");
        assert!(!removed.contains(MANAGED_MARKER));
        assert!(removed.contains("user-notify"));
        assert!(removed.contains("model = \"gpt-5\""));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_paths_and_preview_are_agent_specific() {
        let home = Path::new(r"C:\Users\operator");
        assert_eq!(
            config_path(Agent::Claude, home, None),
            PathBuf::from(r"C:\Users\operator\.claude\settings.json")
        );
        assert_eq!(
            config_path(Agent::Codex, home, Some(Path::new(r"D:\CodexProfile"))),
            PathBuf::from(r"D:\CodexProfile\config.toml")
        );
        assert!(preview(Agent::Claude, Path::new("terminalai.exe")).contains("SessionStart"));
        assert!(preview(Agent::Codex, Path::new("terminalai.exe")).contains("PermissionRequest"));
        assert_eq!(
            command_executable(r#"'C:\Program Files\TerminalAI\terminalai.exe' hook codex"#),
            r"C:\Program Files\TerminalAI\terminalai.exe"
        );
    }

    #[test]
    fn status_reports_stale_managed_commands_and_codex_fallback() {
        let dir = test_dir();
        let claude_path = dir.join("settings.json");
        fs::write(
            &claude_path,
            format!(
                r#"{{"hooks":{{"Stop":[{{"matcher":"","hooks":[{{"type":"command","command":"C:\\old\\terminalai.exe hook claude {MANAGED_MARKER}"}}]}}]}}}}"#
            ),
        )
        .expect("seed stale settings");
        let current = Path::new(r"C:\Program Files\TerminalAI\terminalai.exe");
        let status = status_at(Agent::Claude, &claude_path, current).expect("status");
        assert!(!status.installed);
        assert!(status.stale);

        let codex_path = dir.join("config.toml");
        fs::write(
            &codex_path,
            r#"notify = ['C:\Program Files\TerminalAI\terminalai.exe', "hook", "codex", "--terminalai-managed"]
"#,
        )
        .expect("seed fallback");
        let status = status_at(Agent::Codex, &codex_path, current).expect("fallback status");
        assert!(status.installed);
        assert!(status.fallback_installed);
        assert!(!status.stale);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn codex_disabled_hooks_are_left_untouched() {
        let dir = test_dir();
        let path = dir.join("config.toml");
        let original = "[features]\nhooks = false\n";
        fs::write(&path, original).expect("seed disabled config");
        let executable = Path::new("terminalai.exe");
        assert!(matches!(
            install_at(Agent::Codex, &path, executable),
            Err(HookConfigError::HooksDisabled)
        ));
        assert_eq!(fs::read_to_string(&path).expect("read config"), original);
        let _ = fs::remove_dir_all(dir);
    }
}
