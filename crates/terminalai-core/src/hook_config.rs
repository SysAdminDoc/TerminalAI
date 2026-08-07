//! Explicit, reversible installation of the agent hook adapters.
//!
//! The writer only owns entries carrying [`MANAGED_MARKER`]. Existing user
//! hooks and unrelated Codex notification commands are retained verbatim.
//!
//! # Why this writes to the operator's configuration at all
//!
//! Writing into someone's global agent settings is the most invasive thing this
//! project does, so the alternative was tested rather than assumed. Claude Code
//! grew `--include-hook-events`, which emits hook lifecycle records on the
//! session's own stdout — apparently making this module unnecessary. It does
//! not, for two independent reasons, both measured against Claude Code 2.1.170
//! on 2026-08-07:
//!
//! 1. **The stream reports hooks running, not lifecycle events happening.**
//!    Records arrive as `{"type":"system","subtype":"hook_started"|
//!    "hook_progress"|"hook_response","hook_event":…,"hook_name":…}`, where
//!    `hook_name` is the *configured* entry (`SessionStart:startup`,
//!    `PreToolUse:Read`) and one lifecycle moment yields one record per
//!    installed hook. Re-running the identical prompt under
//!    `--settings '{"disableAllHooks":true}'` produced the same tool call and
//!    the same result with **zero** hook records. So the flag is a view onto
//!    the hooks already installed; it cannot be the reason not to install them.
//! 2. **It is print-mode only.** `--include-hook-events` requires
//!    `--output-format stream-json`, which the CLI documents as working only
//!    with `--print`, and `--print` runs one non-interactive turn. This tool
//!    supervises interactive PTY sessions, which is the mode that has no such
//!    stream.
//!
//! Recorded here rather than in working notes so the next reader who finds that
//! flag does not repeat the run. The policy preflight this module performs —
//! `disableAllHooks`, `allowManagedHooksOnly`, the HKLM managed-settings chain —
//! is therefore not a fallback path but the only path, and finding #1 is
//! precisely why: a policy that stops the hooks from running also silences the
//! stream that would have reported them.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value as JsonValue};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table, Value as TomlValue};

use crate::agent::Agent;
use crate::atomic_file::write_atomic;

pub const MANAGED_MARKER: &str = "--terminalai-managed";

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "Notification",
    "PreToolUse",
    "PermissionRequest",
    "PermissionDenied",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    // The row's folder and branch describe where the session is; an agent that
    // moves invalidates both, and nothing else reports the move.
    "CwdChanged",
    // Answered with a path, so a checkout the agent makes lands under this
    // tool's worktree root instead of becoming a stray for the survey to find.
    "WorktreeCreate",
];

/// Events this tool deliberately does not manage, and why.
///
/// Recorded because "not handled" and "handled by doing nothing" look identical
/// from outside, and the next person to read the managed list should not have to
/// re-derive which is which:
///
/// - `DirectoryAdded` — an *additional* directory, not a move. The row names one
///   working directory by design; showing a session as being in two places is a
///   worse answer than showing the one it was launched in.
/// - `TaskCreated` / `TaskCompleted` — tool progress the row already gets from
///   `TodoWrite` through `PostToolUse`. Ingesting both would double-count a plan.
/// - `Elicitation` / `ElicitationResult` — the agent asking its *own* caller for
///   input, which this tool is not. `PermissionRequest` is the one that concerns
///   the operator and it is already managed.
/// - `MessageDisplay`, `UserPromptExpansion`, `InstructionsLoaded` — content, not
///   state. Ingesting them would put transcript-adjacent text on a row that is
///   deliberately built from status alone.
/// - `ConfigChange` — this tool writes managed hook entries into that same
///   configuration, so ingesting its own writes is a feedback loop.
#[allow(dead_code)]
const DELIBERATELY_UNMANAGED: &[&str] = &[
    "DirectoryAdded",
    "TaskCreated",
    "TaskCompleted",
    "Elicitation",
    "ElicitationResult",
    "MessageDisplay",
    "UserPromptExpansion",
    "InstructionsLoaded",
    "ConfigChange",
];

const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "SubagentStart",
    "SubagentStop",
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
    #[error("managed settings at {location} are invalid JSON: {error}")]
    ManagedSettingsJson { location: String, error: String },
    #[error("managed settings at {location} have an invalid root shape")]
    ManagedSettingsShape { location: String },
    #[error("could not read managed settings at {location}: {error}")]
    ManagedSettingsIo { location: String, error: String },
    #[error("{agent} does not support HTTP hook handlers")]
    UnsupportedTransport { agent: &'static str },
}

/// How TerminalAI delivers normalized hook payloads to the daemon.
///
/// Claude Code supports HTTP handlers, so a live daemon can avoid a process
/// spawn for every hook. Codex currently accepts command handlers only; its
/// callers deliberately use the command variant and retain the existing
/// fail-open CLI adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookTransport {
    Command {
        executable: PathBuf,
    },
    Http {
        url: String,
        host: String,
        bearer_token: String,
    },
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
    pub http_installed: bool,
}

/// The locally observable Claude policy that can prevent TerminalAI's
/// user-level hook entries from running. Remote/server-managed policy is not
/// visible to a local preflight and is intentionally not guessed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedHookPolicy {
    pub sources: Vec<String>,
    pub disable_all_hooks: bool,
    pub allow_managed_hooks_only: bool,
    pub strict_plugin_hooks: bool,
}

impl ManagedHookPolicy {
    pub fn blocks_user_hooks(&self) -> bool {
        self.disable_all_hooks || self.allow_managed_hooks_only || self.strict_plugin_hooks
    }

    pub fn blocking_settings(&self) -> Vec<&'static str> {
        let mut settings = Vec::new();
        if self.disable_all_hooks {
            settings.push("disableAllHooks=true");
        }
        if self.allow_managed_hooks_only {
            settings.push("allowManagedHooksOnly=true");
        }
        if self.strict_plugin_hooks {
            settings.push("strictPluginOnlyCustomization includes hooks");
        }
        settings
    }
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

/// Read the highest-priority locally inspectable Claude managed-settings
/// source. The order mirrors Claude Code's native Windows precedence:
/// machine policy, system managed-settings files, then the user policy key.
/// A source with no hook-blocking key still wins over lower-priority sources;
/// lower policy tiers must not be merged into an effective result.
pub fn managed_hook_policy() -> Result<Option<ManagedHookPolicy>, HookConfigError> {
    #[cfg(windows)]
    {
        managed_hook_policy_windows()
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

fn managed_hook_policy_from_settings(
    sources: Vec<String>,
    settings: JsonValue,
) -> Result<ManagedHookPolicy, HookConfigError> {
    let object = settings.as_object().ok_or_else(|| {
        HookConfigError::ManagedSettingsShape {
            location: sources.join(", "),
        }
    })?;
    let strict_plugin_hooks = match object.get("strictPluginOnlyCustomization") {
        Some(value) if value.as_bool() == Some(true) => true,
        Some(value) => value
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("hooks"))),
        None => false,
    };
    Ok(ManagedHookPolicy {
        sources,
        disable_all_hooks: object
            .get("disableAllHooks")
            .and_then(JsonValue::as_bool)
            == Some(true),
        allow_managed_hooks_only: object
            .get("allowManagedHooksOnly")
            .and_then(JsonValue::as_bool)
            == Some(true),
        strict_plugin_hooks,
    })
}

fn merge_managed_settings(target: &mut JsonValue, overlay: JsonValue) {
    match (target, overlay) {
        (JsonValue::Object(target), JsonValue::Object(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = target.get_mut(&key) {
                    merge_managed_settings(existing, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (JsonValue::Array(target), JsonValue::Array(overlay)) => {
            for value in overlay {
                if !target.iter().any(|existing| existing == &value) {
                    target.push(value);
                }
            }
        }
        (target, overlay) => *target = overlay,
    }
}

fn read_managed_settings_file(path: &Path) -> Result<JsonValue, HookConfigError> {
    let location = path.display().to_string();
    let text = fs::read_to_string(path).map_err(|error| HookConfigError::ManagedSettingsIo {
        location: location.clone(),
        error: error.to_string(),
    })?;
    serde_json::from_str(&text).map_err(|error| HookConfigError::ManagedSettingsJson {
        location,
        error: error.to_string(),
    })
}

fn file_managed_hook_policy(root: &Path) -> Result<Option<ManagedHookPolicy>, HookConfigError> {
    let base = root.join("managed-settings.json");
    let dropins = root.join("managed-settings.d");
    let mut effective = None;
    let mut sources = Vec::new();

    if base.is_file() {
        effective = Some(read_managed_settings_file(&base)?);
        sources.push(base.display().to_string());
    }

    let mut paths = match fs::read_dir(&dropins) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| HookConfigError::ManagedSettingsIo {
                location: dropins.display().to_string(),
                error: error.to_string(),
            })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(HookConfigError::ManagedSettingsIo {
                location: dropins.display().to_string(),
                error: error.to_string(),
            });
        }
    };
    paths.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
    for path in paths {
        let is_json = path
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("json");
        let hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'));
        if !path.is_file() || !is_json || hidden {
            continue;
        }
        let value = read_managed_settings_file(&path)?;
        if let Some(current) = effective.as_mut() {
            merge_managed_settings(current, value);
        } else {
            effective = Some(value);
        }
        sources.push(path.display().to_string());
    }

    effective
        .map(|settings| managed_hook_policy_from_settings(sources, settings))
        .transpose()
}

#[cfg(windows)]
fn managed_hook_policy_windows() -> Result<Option<ManagedHookPolicy>, HookConfigError> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let machine_source = r"HKLM\SOFTWARE\Policies\ClaudeCode\Settings";
    if let Some((source, text)) = read_registry_settings(HKEY_LOCAL_MACHINE, machine_source)? {
        let settings = serde_json::from_str(&text).map_err(|error| {
            HookConfigError::ManagedSettingsJson {
                location: source.clone(),
                error: error.to_string(),
            }
        })?;
        return managed_hook_policy_from_settings(vec![source], settings).map(Some);
    }

    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    if let Some(policy) = file_managed_hook_policy(&program_files.join("ClaudeCode"))? {
        return Ok(Some(policy));
    }

    let user_source = r"HKCU\SOFTWARE\Policies\ClaudeCode\Settings";
    if let Some((source, text)) = read_registry_settings(HKEY_CURRENT_USER, user_source)? {
        let settings = serde_json::from_str(&text).map_err(|error| {
            HookConfigError::ManagedSettingsJson {
                location: source.clone(),
                error: error.to_string(),
            }
        })?;
        return managed_hook_policy_from_settings(vec![source], settings).map(Some);
    }

    Ok(None)
}

#[cfg(windows)]
fn read_registry_settings(
    root: windows_sys::Win32::System::Registry::HKEY,
    source: &str,
) -> Result<Option<(String, String)>, HookConfigError> {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };

    let subkey = wide_null(r"SOFTWARE\Policies\ClaudeCode");
    let value_name = wide_null("Settings");
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut value_type = 0;
    let mut size = 0u32;
    let mut result = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value_name.as_ptr(),
            flags,
            &mut value_type,
            null_mut(),
            &mut size,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if result != ERROR_SUCCESS {
        return Err(HookConfigError::ManagedSettingsIo {
            location: source.to_owned(),
            error: std::io::Error::from_raw_os_error(result as i32).to_string(),
        });
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    result = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value_name.as_ptr(),
            flags,
            &mut value_type,
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut size,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(HookConfigError::ManagedSettingsIo {
            location: source.to_owned(),
            error: std::io::Error::from_raw_os_error(result as i32).to_string(),
        });
    }
    let units = (size as usize / std::mem::size_of::<u16>()).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..units])
        .trim_end_matches('\0')
        .to_owned();
    Ok(Some((source.to_owned(), text)))
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
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
            let handler = claude_handler(&HookTransport::Command {
                executable: executable.to_path_buf(),
            })
            .unwrap_or_else(|_| json!({"type": "command", "command": command, "async": true}));
            let hooks: Map<String, JsonValue> = CLAUDE_EVENTS
                .iter()
                .map(|event| ((*event).into(), json!([claude_group(handler.clone())])))
                .collect();
            serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap_or_default()
        }
        Agent::Codex => {
            let mut doc = DocumentMut::new();
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
    install_at_with_transport(
        agent,
        path,
        &HookTransport::Command {
            executable: executable.to_path_buf(),
        },
    )
}

pub fn install_at_with_transport(
    agent: Agent,
    path: &Path,
    transport: &HookTransport,
) -> Result<HookChange, HookConfigError> {
    let before = read_optional(path)?;
    let after = match agent {
        Agent::Claude => install_claude_text(before.as_deref(), transport)?,
        Agent::Codex => {
            let HookTransport::Command { executable } = transport else {
                return Err(HookConfigError::UnsupportedTransport { agent: "Codex" });
            };
            install_codex_text(before.as_deref(), &command_for(agent, executable))?
        }
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

/// Replace a managed Claude HTTP hook with the command transport on shutdown.
///
/// The HTTP listener and its bearer token are daemon-lifetime state. Keeping
/// either in a global Claude settings file after the listener is gone leaves a
/// dead endpoint and a stale secret in every later Claude run. The command
/// transport has no secret and is fail-open when the daemon is unavailable, so
/// it is the safe durable fallback.
pub fn downgrade_claude_http_at(
    path: &Path,
    executable: &Path,
) -> Result<HookChange, HookConfigError> {
    let Some(before) = read_optional(path)? else {
        return Ok(HookChange {
            path: path.to_path_buf(),
            changed: false,
        });
    };
    let root: JsonValue = serde_json::from_str(&before)?;
    if !contains_managed_claude_http(&root) {
        return Ok(HookChange {
            path: path.to_path_buf(),
            changed: false,
        });
    }
    let after = install_claude_text(
        Some(&before),
        &HookTransport::Command {
            executable: executable.to_path_buf(),
        },
    )?;
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
    status_at_with_transport(
        agent,
        path,
        &HookTransport::Command {
            executable: executable.to_path_buf(),
        },
    )
}

pub fn status_at_with_transport(
    agent: Agent,
    path: &Path,
    transport: &HookTransport,
) -> Result<HookStatus, HookConfigError> {
    let executable = match transport {
        HookTransport::Command { executable } => executable,
        HookTransport::Http { .. } => Path::new("terminalai.exe"),
    };
    let command = command_for(agent, executable);
    let (installed, stale, disabled, fallback_installed, http_installed) =
        match read_optional(path)? {
            None => (false, false, false, false, false),
            Some(text) => match agent {
                Agent::Claude => inspect_claude_text(&text, &command, transport)?,
                Agent::Codex => {
                    let HookTransport::Command { .. } = transport else {
                        return Err(HookConfigError::UnsupportedTransport { agent: "Codex" });
                    };
                    let (installed, stale, disabled, fallback) =
                        inspect_codex_text(&text, &command)?;
                    (installed, stale, disabled, fallback, false)
                }
            },
        };
    Ok(HookStatus {
        agent,
        path: path.to_path_buf(),
        installed: installed || fallback_installed || http_installed,
        stale,
        disabled,
        fallback_installed,
        http_installed,
    })
}

fn install_claude_text(
    text: Option<&str>,
    transport: &HookTransport,
) -> Result<String, HookConfigError> {
    let mut root = match text {
        Some(text) => serde_json::from_str::<JsonValue>(text)?,
        None => json!({}),
    };
    let object = root.as_object_mut().ok_or(HookConfigError::InvalidShape {
        agent: "Claude",
        field: "root",
    })?;
    let handler = claude_handler(transport)?;
    {
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
            groups.push(claude_group(handler.clone()));
        }
    }
    update_claude_allowlist(object, transport)?;
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

fn remove_claude_text(text: &str) -> Result<String, HookConfigError> {
    let mut root: JsonValue = serde_json::from_str(text)?;
    let Some(hooks) = root.get_mut("hooks").and_then(JsonValue::as_object_mut) else {
        update_claude_allowlist(
            root.as_object_mut().ok_or(HookConfigError::InvalidShape {
                agent: "Claude",
                field: "root",
            })?,
            &HookTransport::Command {
                executable: PathBuf::new(),
            },
        )?;
        return Ok(serde_json::to_string_pretty(&root)? + "\n");
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
    if let Some(object) = root.as_object_mut() {
        update_claude_allowlist(
            object,
            &HookTransport::Command {
                executable: PathBuf::new(),
            },
        )?;
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
        handlers.retain(|handler| !is_managed_claude_handler(handler, agent));
        !handlers.is_empty()
    });
    before != groups.len()
}

fn claude_group(handler: JsonValue) -> JsonValue {
    json!({
        "matcher": "",
        "hooks": [handler]
    })
}

fn claude_handler(transport: &HookTransport) -> Result<JsonValue, HookConfigError> {
    match transport {
        HookTransport::Command { executable } => Ok(json!({
            "type": "command",
            "command": command_for(Agent::Claude, executable),
            "async": true
        })),
        HookTransport::Http {
            url, bearer_token, ..
        } => Ok(json!({
            "type": "http",
            "url": url,
            "timeout": 2,
            "headers": {
                "Authorization": format!("Bearer {bearer_token}"),
                "X-TerminalAI-Managed": MANAGED_MARKER
            }
        })),
    }
}

fn update_claude_allowlist(
    object: &mut Map<String, JsonValue>,
    transport: &HookTransport,
) -> Result<(), HookConfigError> {
    let desired = match transport {
        HookTransport::Http { url, .. } => Some(url.as_str()),
        HookTransport::Command { .. } => None,
    };
    let Some(existing) = object.get("allowedHttpHookUrls") else {
        if let Some(url) = desired {
            object.insert("allowedHttpHookUrls".into(), json!([url]));
        }
        return Ok(());
    };
    let Some(values) = existing.as_array() else {
        if desired.is_none() {
            return Ok(());
        }
        return Err(HookConfigError::InvalidShape {
            agent: "Claude",
            field: "allowedHttpHookUrls",
        });
    };
    let mut urls = Vec::new();
    for value in values {
        let Some(value) = value.as_str() else {
            return Err(HookConfigError::InvalidShape {
                agent: "Claude",
                field: "allowedHttpHookUrls entry",
            });
        };
        if !is_terminalai_http_url(value) && !urls.iter().any(|url| url == value) {
            urls.push(value.to_owned());
        }
    }
    if let Some(url) = desired {
        urls.push(url.to_owned());
    }
    object.insert("allowedHttpHookUrls".into(), json!(urls));
    Ok(())
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

fn install_codex_document(doc: &mut DocumentMut, command: &str) -> bool {
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

fn remove_codex_document(doc: &mut DocumentMut, agent: Agent) -> bool {
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

fn parse_codex(text: Option<&str>) -> Result<DocumentMut, HookConfigError> {
    match text {
        Some(text) => text
            .parse::<DocumentMut>()
            .map_err(|error| HookConfigError::Toml(error.to_string())),
        None => Ok(DocumentMut::new()),
    }
}

fn codex_hooks_disabled(doc: &DocumentMut) -> bool {
    doc.get("features")
        .and_then(Item::as_table)
        .and_then(|features| features.get("hooks"))
        .and_then(Item::as_value)
        .and_then(|value| value.as_bool())
        == Some(false)
}

fn is_managed_claude_handler(handler: &JsonValue, agent: Agent) -> bool {
    handler
        .get("command")
        .and_then(JsonValue::as_str)
        .is_some_and(|command| is_managed_command(command, agent))
        || (agent == Agent::Claude
            && handler.get("type").and_then(JsonValue::as_str) == Some("http")
            && handler
                .get("headers")
                .and_then(JsonValue::as_object)
                .and_then(|headers| headers.get("X-TerminalAI-Managed"))
                .and_then(JsonValue::as_str)
                == Some(MANAGED_MARKER))
}

fn is_terminalai_http_url(value: &str) -> bool {
    value.starts_with("http://127.0.0.1:") && value.contains("/hooks/")
}

fn contains_managed_claude_http(root: &JsonValue) -> bool {
    let has_handler = root
        .get("hooks")
        .and_then(JsonValue::as_object)
        .into_iter()
        .flat_map(|hooks| hooks.values())
        .filter_map(JsonValue::as_array)
        .flat_map(|groups| groups.iter())
        .filter_map(|group| group.get("hooks"))
        .filter_map(JsonValue::as_array)
        .flat_map(|handlers| handlers.iter())
        .any(|handler| {
            handler.get("type").and_then(JsonValue::as_str) == Some("http")
                && is_managed_claude_handler(handler, Agent::Claude)
        });
    has_handler || root
        .get("allowedHttpHookUrls")
        .and_then(JsonValue::as_array)
        .is_some_and(|urls| {
            urls.iter()
                .filter_map(JsonValue::as_str)
                .any(is_terminalai_http_url)
        })
}

fn matches_http_transport(handler: &JsonValue, transport: &HookTransport) -> bool {
    let HookTransport::Http {
        url,
        bearer_token,
        host,
    } = transport
    else {
        return false;
    };
    let Some(handler_url) = handler.get("url").and_then(JsonValue::as_str) else {
        return false;
    };
    let authorization = handler
        .get("headers")
        .and_then(JsonValue::as_object)
        .and_then(|headers| headers.get("Authorization"))
        .and_then(JsonValue::as_str);
    let expected_authorization = format!("Bearer {bearer_token}");
    handler_url == url
        && url.starts_with(&format!("http://{host}/"))
        && authorization == Some(expected_authorization.as_str())
}

fn inspect_claude_text(
    text: &str,
    command: &str,
    transport: &HookTransport,
) -> Result<(bool, bool, bool, bool, bool), HookConfigError> {
    let root: JsonValue = serde_json::from_str(text)?;
    let mut current_command = false;
    let mut current_http = false;
    let mut stale = false;
    if let Some(hooks) = root.get("hooks").and_then(JsonValue::as_object) {
        for groups in hooks.values().filter_map(JsonValue::as_array) {
            for group in groups {
                if let Some(handlers) = group.get("hooks").and_then(JsonValue::as_array) {
                    for handler in handlers {
                        if !is_managed_claude_handler(handler, Agent::Claude) {
                            continue;
                        }
                        if let Some(value) = handler.get("command").and_then(JsonValue::as_str) {
                            if matches!(transport, HookTransport::Command { .. })
                                && value == command
                            {
                                current_command = true;
                            } else {
                                stale = true;
                            }
                        } else if handler.get("type").and_then(JsonValue::as_str) == Some("http") {
                            if matches_http_transport(handler, transport) {
                                current_http = true;
                            } else {
                                stale = true;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok((
        current_command || current_http,
        stale,
        false,
        false,
        current_http,
    ))
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
    write_atomic(path, after.as_bytes(), true)?;
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
    fn claude_http_install_covers_lifecycle_and_falls_back_to_command() {
        let dir = test_dir();
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"allowedHttpHookUrls":["http://user.example/hook"]}"#,
        )
        .expect("seed settings");
        let transport = HookTransport::Http {
            url: "http://127.0.0.1:43123/hooks/claude".into(),
            host: "127.0.0.1:43123".into(),
            bearer_token: "token".into(),
        };
        install_at_with_transport(Agent::Claude, &path, &transport).expect("install HTTP");
        let installed = fs::read_to_string(&path).expect("read settings");
        let root: JsonValue = serde_json::from_str(&installed).expect("settings JSON");
        for event in CLAUDE_EVENTS {
            assert_eq!(root["hooks"][*event][0]["hooks"][0]["type"], "http");
        }
        assert_eq!(
            root["hooks"]["PostToolUseFailure"][0]["hooks"][0]["headers"]["Authorization"],
            "Bearer token"
        );
        assert!(root["allowedHttpHookUrls"]
            .as_array()
            .expect("allowlist")
            .iter()
            .any(|value| value == "http://user.example/hook"));
        assert!(root["allowedHttpHookUrls"]
            .as_array()
            .expect("allowlist")
            .iter()
            .any(|value| value == "http://127.0.0.1:43123/hooks/claude"));
        let status = status_at_with_transport(Agent::Claude, &path, &transport).expect("status");
        assert!(status.installed);
        assert!(status.http_installed);
        assert!(!status.stale);

        let executable = Path::new(r"C:\\Program Files\\TerminalAI\\terminalai.exe");
        install_at(Agent::Claude, &path, executable).expect("command fallback");
        let fallback = fs::read_to_string(&path).expect("read fallback settings");
        assert!(!fallback.contains("\"type\": \"http\""));
        assert!(!fallback.contains("127.0.0.1:43123"));
        assert!(fallback.contains(MANAGED_MARKER));
        assert!(fallback.contains("user.example"));
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
    fn managed_hook_policy_detects_every_user_hook_blocker() {
        let policy = managed_hook_policy_from_settings(
            vec![r"HKLM\SOFTWARE\Policies\ClaudeCode\Settings".into()],
            json!({
                "disableAllHooks": true,
                "allowManagedHooksOnly": true,
                "strictPluginOnlyCustomization": ["hooks"]
            }),
        )
        .expect("policy");
        assert!(policy.blocks_user_hooks());
        assert_eq!(
            policy.blocking_settings(),
            vec![
                "disableAllHooks=true",
                "allowManagedHooksOnly=true",
                "strictPluginOnlyCustomization includes hooks"
            ]
        );
    }

    #[test]
    fn managed_settings_dropins_are_merged_in_name_order() {
        let dir = test_dir();
        let dropins = dir.join("managed-settings.d");
        fs::create_dir_all(&dropins).expect("dropins");
        fs::write(
            dir.join("managed-settings.json"),
            r#"{"disableAllHooks":false,"strictPluginOnlyCustomization":["hooks"],"nested":{"keep":true}}"#,
        )
        .expect("base settings");
        fs::write(
            dropins.join("20-hooks.json"),
            r#"{"disableAllHooks":true,"strictPluginOnlyCustomization":["skills"]}"#,
        )
        .expect("hook policy");
        fs::write(
            dropins.join(".ignored.json"),
            r#"{"disableAllHooks":false}"#,
        )
        .expect("hidden settings");

        let policy = file_managed_hook_policy(&dir)
            .expect("read policy")
            .expect("policy exists");
        assert!(policy.blocks_user_hooks());
        assert_eq!(policy.sources.len(), 2);
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
