//! Build-time Tauri metadata.
//!
//! Declaring an `AppManifest` turns each `#[tauri::command]` into an explicit
//! `allow-*` permission the capability file has to grant. Without it there is no
//! declared policy for these commands at all — only the runtime patch Tauri
//! 2.11.1 added — so a future regression, or any remote-origin capability, would
//! have nothing to fail closed against.
//!
//! Adding a command to `generate_handler!` without adding it here fails the
//! build, which is the point: the list cannot silently drift.

/// Every `#[tauri::command]` exposed by this application. Keep in step with
/// `generate_handler!` in `src/main.rs`.
const COMMANDS: &[&str] = &[
    "app_version",
    "fleet_snapshot",
    "review_snapshot",
    "external_sessions",
    "session_history",
    "search_fleet",
    "stale_worktrees",
    "reap_worktree",
    "mark_reviewed",
    "admission_config",
    "set_admission",
    "land_session",
    "preview_launch",
    "resolve_agent",
    "agent_capabilities",
    "launch_session",
    "write_session",
    "resize_session",
    "kill_session",
    "focus_session",
    "mark_read",
    "toggle_pin",
    "grid_snapshot",
    "subscribe_output",
    "stream_scrollback",
    "stream_scrollback_history",
    "broadcast_prompt",
    "queued_prompts",
    "enqueue_prompt",
    "edit_queued_prompt",
    "remove_queued_prompt",
    "reorder_queued_prompt",
    "pause_queue",
    "resume_queue",
    "attach_session_output",
    "revive_session",
    "archive_session",
    "list_presets",
    "list_templates",
    "save_preset",
    "delete_preset",
    "restore_builtin_presets",
    "list_projects",
    "scan_projects",
    "list_stored_prompts",
    "save_stored_prompt",
    "delete_stored_prompt",
    "work_run",
    "start_work_run",
    "approve_flagged_project",
    "skip_work_project",
    "set_work_run_paused",
    "clear_work_run",
    "list_project_roots",
    "add_project_root",
    "remove_project_root",
    "pick_folder",
    "pick_extra_dirs",
    "preflight_report",
    "preflight_fix",
    "open_external_url",
];

const CAPABILITY_FILES: &[&str] = &[
    "./capabilities/default.json",
    "./capabilities/wdio.json",
    "./capabilities/wdio-embedded.json",
];

/// Read the command identifiers from the one `generate_handler!` invocation
/// in `src/main.rs`. Keeping this check source-based makes it cover the actual
/// registration site rather than another hand-maintained list.
fn registered_commands(source: &str) -> Vec<String> {
    const MARKER: &str = "tauri::generate_handler![";
    let start = source
        .find(MARKER)
        .unwrap_or_else(|| panic!("could not find {MARKER:?} in src/main.rs"))
        + MARKER.len();
    let end = source[start..]
        .find(']')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("generate_handler! in src/main.rs has no closing bracket"));
    source[start..end]
        .split(',')
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Fail when the source registration and the manifest list drift in either
/// direction. A one-way check lets a removed handler remain in the manifest,
/// which still makes the capability surface claim a command exists.
fn assert_manifest_matches_handler(source: &str) {
    use std::collections::BTreeSet;

    let registered = registered_commands(source);
    let registered_set: BTreeSet<&str> = registered.iter().map(String::as_str).collect();
    let manifest_set: BTreeSet<&str> = COMMANDS.iter().copied().collect();
    let missing: Vec<&str> = registered_set
        .difference(&manifest_set)
        .copied()
        .collect();
    let stale: Vec<&str> = manifest_set
        .difference(&registered_set)
        .copied()
        .collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "Tauri command manifest drifted from generate_handler!: missing in COMMANDS={missing:?}, stale in COMMANDS={stale:?}"
    );
    assert_eq!(
        registered.len(),
        registered_set.len(),
        "generate_handler! contains a duplicate command"
    );
}

/// Fail the build when a command has no matching `allow-*` grant.
///
/// Declaring the manifest alone is not enough: the ACL is enforced at *invoke*
/// time, so a command added to `COMMANDS` but never granted builds cleanly and
/// then fails in front of a user. This turns that into a build error.
fn assert_every_command_is_granted(capabilities: &str) {
    let path = std::path::Path::new(capabilities);
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
    let granted: Vec<String> = text
        .split('"')
        .filter(|token| token.starts_with("allow-"))
        .map(str::to_owned)
        .collect();
    let missing: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .filter(|command| {
            let permission = format!("allow-{}", command.replace('_', "-"));
            !granted.iter().any(|grant| grant == &permission)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "{} grants no permission for these commands: {missing:?}. Add `allow-<command>` \
         (hyphenated) to its permissions array — the ACL is checked when the command is \
         invoked, so without this the failure would reach a user instead of the build.",
        path.display(),
    );
}

fn main() {
    let handler_source = std::fs::read_to_string("src/main.rs")
        .unwrap_or_else(|error| panic!("could not read src/main.rs: {error}"));
    println!("cargo:rerun-if-changed=src/main.rs");
    assert_manifest_matches_handler(&handler_source);
    for capabilities in CAPABILITY_FILES {
        assert_every_command_is_granted(capabilities);
    }
    let selected_capabilities = if std::env::var_os("CARGO_FEATURE_WDIO_EMBEDDED").is_some() {
        CAPABILITY_FILES[2]
    } else if std::env::var_os("CARGO_FEATURE_WDIO").is_some() {
        CAPABILITY_FILES[1]
    } else {
        CAPABILITY_FILES[0]
    };
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .capabilities_path_pattern(selected_capabilities)
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to build Tauri application metadata");
}
