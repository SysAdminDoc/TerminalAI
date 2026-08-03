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
    "mark_reviewed",
    "preview_launch",
    "resolve_agent",
    "launch_session",
    "write_session",
    "resize_session",
    "kill_session",
    "focus_session",
    "mark_read",
    "toggle_pin",
    "subscribe_output",
    "stream_scrollback",
    "attach_session_output",
    "revive_session",
    "archive_session",
    "list_presets",
    "save_preset",
    "delete_preset",
    "pick_folder",
    "pick_extra_dirs",
    "preflight_report",
    "preflight_fix",
    "open_external_url",
];

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
    let capabilities = if std::env::var_os("CARGO_FEATURE_WDIO_EMBEDDED").is_some() {
        "./capabilities/wdio-embedded.json"
    } else if std::env::var_os("CARGO_FEATURE_WDIO").is_some() {
        "./capabilities/wdio.json"
    } else {
        "./capabilities/default.json"
    };
    assert_every_command_is_granted(capabilities);
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .capabilities_path_pattern(capabilities)
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to build Tauri application metadata");
}
