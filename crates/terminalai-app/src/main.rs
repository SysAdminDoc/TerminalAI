#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dpi;
mod app_lifecycle;
mod daemon;
mod events;
mod output;
mod preset;
mod projects;
mod preflight;
mod restart;
mod session_commands;
mod state;
mod toast;
mod work;
mod workflows;
mod workingset;
mod workspace_commands;

use terminalai_core::schedule::{FiringResult, ScheduleFiring, WorkSchedule};
use terminalai_core::work_queue::{EntryState, WorkQueue};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};
use std::{io, io::Read};

use tauri::{Emitter, Manager, State};
use terminalai_core::agent::Agent;
use terminalai_core::launch::LaunchSpec;
use terminalai_core::{
    fleet_progress, parse_hook_in, FleetProgress, HookTransport, LogEntry,
    ProgressStatus, RegistryEvent, Session, SessionId, SessionStatus, TaskProgress, MAX_LOG_ENTRIES,
};
use terminalai_daemon::{DaemonClient, HookEndpoint, Request, Response, PROTOCOL_VERSION};
use daemon::{client as daemon_client, response as daemon_response, run_blocking};
use app_lifecycle::{
    cleanup_http_hooks, cleanup_http_hooks_at, connect_for_app, connect_or_start_daemon,
    install_daemon_client,
};
use preflight::{open_external_url, preflight_fix, preflight_report};
use output::{replay_overlap, OutputChannels};
use session_commands::{
    agent_capabilities, archive_session, attach_session_output, broadcast_prompt, edit_queued_prompt,
    enqueue_prompt, focus_session, grid_snapshot, kill_session, land_session, launch_session,
    mark_read, mark_reviewed, pause_queue, preview_launch, queued_prompts, remove_queued_prompt,
    reorder_queued_prompt, resize_session, revive_session, resume_queue, stream_scrollback,
    stream_scrollback_history, subscribe_output, toggle_pin, write_session,
};
use workflows::{
    approve_flagged_project, clear_work_run, clear_work_schedule, finish_work_run_session,
    fire_due_schedule, set_work_run_paused, set_work_schedule, set_work_schedule_paused,
    skip_work_project, start_work_run, work_run, work_schedule,
};
use workspace_commands::{
    add_project_root, delete_preset, delete_stored_prompt, delete_working_set, list_presets,
    list_project_roots, list_projects, list_stored_prompts, list_templates, list_working_sets,
    pick_extra_dirs, pick_folder, reap_worktree, remove_project_root, restore_builtin_presets,
    restore_working_set, save_preset, save_stored_prompt, save_working_set, scan_projects,
    search_fleet, stale_worktrees,
};
use state::{
    AppState, FleetSnapshot, PreflightCheck, PreflightReport, ReviewSnapshot, APP_USER_MODEL_ID,
    PREFLIGHT_DAEMON_TIMEOUT,
};

#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[tauri::command]
fn fleet_snapshot(state: State<'_, AppState>) -> Result<FleetSnapshot, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::Snapshot)? {
        Response::Snapshot {
            sessions,
            focused,
            admission,
            store_quarantine,
            store_write_error,
        } => Ok(FleetSnapshot {
            sessions,
            focused,
            admission,
            store_quarantine,
            store_write_error,
        }),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected snapshot response: {other:?}")),
    }
}

#[tauri::command]
fn review_snapshot(state: State<'_, AppState>) -> Result<ReviewSnapshot, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::ReviewSnapshot)? {
        Response::ReviewSnapshot { entries } => Ok(ReviewSnapshot { entries }),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected review response: {other:?}")),
    }
}

/// Sessions running outside this supervisor. Read-only by construction: the
/// response carries no handle the UI could act on.
#[tauri::command]
async fn external_sessions(
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::ExternalSession>, String> {
    let client = daemon_client(&state)?;
    run_blocking("external_sessions", move || {
        match daemon_response(&client, Request::ExternalSessions)? {
            Response::ExternalSessions { sessions } => Ok(sessions),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected external-session response: {other:?}")),
        }
    })
    .await
}

/// Sessions this supervisor finished, newest first.
///
/// The archive has always been written and never read back for anything but the
/// id counter. It carries no PTY handle and no output — only what is needed to
/// see what ran and to start the same thing again.
#[tauri::command]
async fn session_history(
    state: State<'_, AppState>,
) -> Result<Vec<terminalai_core::ArchivedSession>, String> {
    let client = daemon_client(&state)?;
    run_blocking("session_history", move || {
        match daemon_response(&client, Request::SessionHistory)? {
            Response::SessionHistory { archives } => Ok(archives),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected session-history response: {other:?}")),
        }
    })
    .await
}

/// Read the daemon-wide admission policy for the settings dialog.
#[tauri::command]
fn admission_config(
    state: State<'_, AppState>,
) -> Result<terminalai_daemon::AdmissionSettings, String> {
    let client = daemon_client(&state)?;
    match daemon_response(&client, Request::AdmissionConfig)? {
        Response::Admission { admission } => Ok(admission),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected admission response: {other:?}")),
    }
}

/// Replace the daemon-wide admission policy without restarting it.
#[tauri::command]
fn set_admission(
    settings: terminalai_daemon::AdmissionSettings,
    state: State<'_, AppState>,
) -> Result<terminalai_daemon::AdmissionSettings, String> {
    let client = daemon_client(&state)?;
    let request = Request::SetAdmission {
        max_live_sessions: settings.max_live_sessions,
        default_budget_usd: settings.default_budget_usd,
        spend_ceiling_usd: settings.spend_ceiling_usd,
        spend_window_hours: Some(settings.spend_window_hours),
        memory_budget_mb: settings.memory_budget_mb,
        session_memory_cap_mb: settings.session_memory_cap_mb,
        max_processes_per_session: settings.max_processes_per_session,
    };
    // Reported by the daemon, never sent back: the boot environment is not the
    // dialog's to rewrite.
    match daemon_response(&client, request)? {
        Response::Admission { admission } => Ok(admission),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected admission response: {other:?}")),
    }
}

fn run_app() -> Result<(), String> {
    #[cfg(feature = "wdio")]
    {
        // tauri-driver passes this through msedgedriver, but EdgeDriver may not
        // preserve it when it launches the application binary. Set it before
        // Tauri creates the first WebView2 environment so external and embedded
        // WDIO providers have the same automation contract.
        std::env::set_var("TAURI_AUTOMATION", "true");
        std::env::set_var("TAURI_WEBVIEW_AUTOMATION", "true");
    }
    #[cfg_attr(not(feature = "wdio"), allow(unused_mut))]
    let mut builder = tauri::Builder::default();
    #[cfg(feature = "wdio")]
    {
        builder = builder.plugin(tauri_plugin_wdio::init());
    }
    #[cfg(feature = "wdio-embedded")]
    {
        builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    }
    let app_result = builder
        .invoke_handler(tauri::generate_handler![
            app_version,
            fleet_snapshot,
            review_snapshot,
            external_sessions,
            session_history,
            search_fleet,
            list_working_sets,
            save_working_set,
            delete_working_set,
            restore_working_set,
            stale_worktrees,
            reap_worktree,
            mark_reviewed,
            admission_config,
            set_admission,
            land_session,
            preview_launch,
            agent_capabilities,
            launch_session,
            write_session,
            resize_session,
            kill_session,
            focus_session,
            mark_read,
            toggle_pin,
            grid_snapshot,
            subscribe_output,
            stream_scrollback,
            stream_scrollback_history,
            broadcast_prompt,
            queued_prompts,
            enqueue_prompt,
            edit_queued_prompt,
            remove_queued_prompt,
            reorder_queued_prompt,
            pause_queue,
            resume_queue,
            attach_session_output,
            revive_session,
            archive_session,
            list_presets,
            list_templates,
            save_preset,
            delete_preset,
            restore_builtin_presets,
            list_projects,
            scan_projects,
            list_stored_prompts,
            save_stored_prompt,
            delete_stored_prompt,
            work_run,
            start_work_run,
            approve_flagged_project,
            skip_work_project,
            set_work_run_paused,
            clear_work_run,
            work_schedule,
            set_work_schedule,
            set_work_schedule_paused,
            clear_work_schedule,
            list_project_roots,
            add_project_root,
            remove_project_root,
            pick_folder,
            pick_extra_dirs,
            preflight_report,
            preflight_fix,
            open_external_url
        ])
        .setup(move |app| app_lifecycle::setup(app, connect_for_app()))
        .run(tauri::generate_context!())
        .map_err(|error| error.to_string());
    let cleanup_result = cleanup_http_hooks();
    match (app_result, cleanup_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error),
    }
}

fn is_hook_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == "hook")
}

/// Deliver an agent hook without initializing Tauri/WebView2.
///
/// Hook commands are deliberately fail-open: an unavailable desktop daemon
/// must never stall or fail the user's agent command. Claude's async hook
/// support normally keeps this off the agent's critical path; the short
/// timeout also bounds Codex's synchronous command hook.
fn run_hook_cli(args: &[String]) -> i32 {
    let Some(agent_arg) = args.first() else {
        eprintln!("usage: terminalai hook <claude|codex>");
        return 0;
    };
    let agent = match agent_arg.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => {
            eprintln!("ignoring hook for unknown agent: {other}");
            return 0;
        }
    };
    let mut input = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut input) {
        eprintln!("ignoring hook input read failure: {error}");
        return 0;
    }
    let event = match parse_hook_in(agent, &input, std::env::current_dir().ok()) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("ignoring malformed hook input: {error}");
            return 0;
        }
    };
    let timeout = Duration::from_millis(750);
    let client = match DaemonClient::connect_with_timeout(timeout) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("ignoring hook because TerminalAI is unavailable: {error}");
            return 0;
        }
    };
    let hook_token = std::env::var("TERMINALAI_HOOK_TOKEN").ok();
    match client.call_with_timeout(
        Request::Hook { event, hook_token },
        timeout,
    ) {
        Ok(Response::Hook { .. }) | Ok(Response::Ok) => {}
        Ok(other) => eprintln!("ignoring unexpected hook response: {other:?}"),
        Err(error) => eprintln!("ignoring hook delivery failure: {error}"),
    }
    0
}

fn main() {
    let _logging = terminalai_daemon::init_logging_with_prefix("terminalai-app");
    // Before anything creates a window, which is the documented requirement and
    // the reason this is the first thing after logging. Awareness is a process
    // property decided by whoever declares it first; leaving it inherited meant
    // every monitor and window measurement was virtualized on a 125% display
    // while still looking plausible.
    dpi::declare_and_report();
    terminalai_daemon::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if is_hook_invocation(&args) {
        std::process::exit(run_hook_cli(&args[1..]));
    }
    // After the hook branch, so a hook invocation never registers: it is a
    // short-lived adapter, and restarting one would relaunch it with no stdin.
    if restart::was_restarted(&args) {
        tracing::info!("relaunched by Windows after a crash, hang or update");
    }
    restart::register();
    if let Err(error) = run_app() {
        eprintln!("TerminalAI: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_schemes_are_opened_from_session_output() {
        // Every one of these is a URI a session could emit inside an OSC 8
        // sequence, and each would be handed to ShellExecute without the check.
        for refused in [
            "file:///C:/Windows/System32/calc.exe",
            "vbscript:msgbox(1)",
            "javascript:alert(1)",
            "ms-msdt:/id",
            "search-ms:query=x",
            r"\\attacker\share\payload.exe",
            "C:\\Windows\\System32\\calc.exe",
            "http://",
            "",
        ] {
            let error = preflight::validate_external_url(refused)
                .expect_err(&format!("{refused:?} must not be opened"));
            assert!(error.starts_with("refused"), "{refused:?} gave {error}");
        }
    }

    #[test]
    fn a_link_carrying_control_characters_is_refused() {
        let error = preflight::validate_external_url("https://example.com/\u{1b}]0;pwned\u{7}")
            .expect_err("control characters must be refused");
        assert!(error.contains("control characters"), "{error}");
    }

    #[test]
    fn ordinary_links_are_opened() {
        // Uppercase spellings are legal URI syntax; refusing them would send an
        // operator chasing a link that looks identical to a working one.
        for allowed in [
            "https://example.com/path?q=1#frag",
            "http://localhost:3000/",
            "HTTPS://Example.COM",
            "mailto:someone@example.com",
            "  https://example.com/padded  ",
        ] {
            let target = preflight::validate_external_url(allowed)
                .unwrap_or_else(|error| panic!("{allowed:?} should open: {error}"));
            assert_eq!(target, allowed.trim());
        }
    }

    #[test]
    fn protocol_version_is_pinned_for_the_shell() {
        assert_eq!(PROTOCOL_VERSION, 4);
    }

    #[test]
    fn attaching_mid_stream_replays_each_pty_byte_once_in_order() {
        let replay = b"prompt\r\noutput> ".to_vec();
        let pending = b"output> next\r\n".to_vec();
        let overlap = replay_overlap(&replay, &pending);
        let mut rendered = replay;
        rendered.extend_from_slice(&pending[overlap..]);

        assert_eq!(rendered, b"prompt\r\noutput> next\r\n");
        assert_eq!(replay_overlap(b"abc", b"xyz"), 0);
    }

    #[test]
    fn hook_invocation_bypasses_the_gui_shell() {
        assert!(is_hook_invocation(&["hook".into(), "claude".into()]));
        assert!(!is_hook_invocation(&["--help".into()]));
        assert!(!is_hook_invocation(&[]));
    }

    #[test]
    fn managed_hook_policy_is_a_distinct_non_fixable_preflight_state() {
        let policy = terminalai_core::ManagedHookPolicy {
            sources: vec![r"C:\Program Files\ClaudeCode\managed-settings.json".into()],
            disable_all_hooks: true,
            allow_managed_hooks_only: false,
            strict_plugin_hooks: false,
        };
        let check = preflight::blocked_hook_preflight(&policy, true, "Claude: installed");
        assert_eq!(check.state, "blocked");
        assert!(!check.can_fix);
        assert_eq!(
            check.detected,
            r"hooks installed but disabled by policy at C:\Program Files\ClaudeCode\managed-settings.json"
        );
        assert!(check
            .detail
            .expect("policy detail")
            .contains("disableAllHooks=true"));
    }

    #[test]
    fn hook_preflight_does_not_equate_configuration_with_delivery() {
        assert_eq!(
            preflight::preflight_hook_state(true, false, false, false),
            "installed, not yet proven"
        );
        assert_eq!(
            preflight::preflight_hook_state(true, false, false, true),
            "installed and firing"
        );
        assert_eq!(preflight::preflight_hook_state(false, false, false, true), "missing");
        assert_eq!(preflight::preflight_hook_state(true, true, false, true), "disabled");
        assert_eq!(preflight::preflight_hook_state(true, false, true, true), "stale");
    }

    #[test]
    fn an_http_hook_handler_is_removed_when_its_endpoint_dies() {
        let home = std::env::temp_dir().join(format!(
            "terminalai-app-hooks-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let path = home.join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().expect("settings parent")).expect("home");
        let transport = HookTransport::Http {
            url: "http://127.0.0.1:43123/hooks/claude".into(),
            host: "127.0.0.1:43123".into(),
            bearer_token: "shutdown-token".into(),
        };
        terminalai_core::install_hooks_at_with_transport(Agent::Claude, &path, &transport)
            .expect("install HTTP hook");

        cleanup_http_hooks_at(&home, Path::new("terminalai.exe")).expect("shutdown cleanup");

        let cleaned = std::fs::read_to_string(&path).expect("read cleaned settings");
        assert!(!cleaned.contains("127.0.0.1"));
        assert!(!cleaned.contains("Bearer shutdown-token"));
        assert!(!cleaned.contains("\"type\": \"http\""));
        assert!(cleaned.contains(terminalai_core::MANAGED_MARKER));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_schedule_does_not_fire_over_a_run_that_is_still_going() {
        // Starting a run replaces the previous one. Firing on top of forty
        // working projects would destroy the report the operator was going to
        // read and put a second agent on the first one's uncommitted edits.
        assert_eq!(
            workflows::previous_run_blocking(None),
            None,
            "a schedule with no run behind it must fire"
        );

        let mut queue = WorkQueue::new(
            "Drain the roadmap",
            &[("shop".into(), PathBuf::from("/repos/shop"))],
        )
        .expect("queue");
        assert!(
            workflows::previous_run_blocking(Some(&queue)).is_some(),
            "a pending run was overwritten by a firing"
        );
        queue
            .set_state(Path::new("/repos/shop"), EntryState::Skipped)
            .expect("state");
        assert!(queue.is_finished());
        assert_eq!(
            workflows::previous_run_blocking(Some(&queue)),
            None,
            "a finished run is a report, and replacing it is what the next firing is for"
        );
    }

    #[test]
    fn only_sessions_that_reported_progress_reach_the_taskbar() {
        // The taskbar is fed from the rows themselves, so a fleet where no
        // agent emits the sequence has to produce no bar rather than a bar at
        // zero -- which would read as "started and got nowhere".
        let spec = terminalai_core::launch::spec_for(Agent::Claude, Path::new("."));
        let quiet = Session::new(SessionId::new(1), &spec);
        let mut reporting = Session::new(SessionId::new(2), &spec);
        reporting.task_progress = Some(TaskProgress::Value { percent: 55 });

        let reported = events::reporting_progress(&[quiet.clone(), reporting]);
        assert_eq!(reported.len(), 1, "a silent session claimed progress");
        assert_eq!(
            fleet_progress(reported.values().copied()),
            Some(FleetProgress {
                status: ProgressStatus::Normal,
                percent: Some(55),
            })
        );
        assert_eq!(
            fleet_progress(events::reporting_progress(&[quiet]).values().copied()),
            None,
            "a fleet that reported nothing produced a bar"
        );
    }
}
