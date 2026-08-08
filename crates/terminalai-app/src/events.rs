//! Daemon event fan-out and desktop attention plumbing.
//!
//! This module owns the long-lived bridge between registry events and the
//! Tauri window, taskbar, toast, and buffered output surfaces.
use super::*;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const OUTPUT_BATCH_INTERVAL: Duration = Duration::from_millis(12);
const LOG_BATCH_INTERVAL: Duration = Duration::from_millis(100);
/// How often a standing schedule is asked whether it is due. Coarse on purpose:
/// the shortest cadence a schedule may have is fifteen minutes, so a check per
/// minute is already an order of magnitude finer than anything it can express.
const SCHEDULE_CHECK_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) fn bridge_daemon_events(
    app: &tauri::AppHandle,
    client: &DaemonClient,
    output_channels: OutputChannels,
    work_run_store: work::WorkRunStore,
    work_schedule_store: work::WorkScheduleStore,
    prompts: work::PromptLibrary,
) {
    let initial_sessions = client
        .call_with_timeout(Request::Snapshot, Duration::from_secs(2))
        .ok()
        .and_then(|response| match response {
            Response::Snapshot { sessions, .. } => Some(sessions),
            _ => None,
        })
        .unwrap_or_default();
    let initial_waiting = waiting_sessions(&initial_sessions);
    let initial_progress = reporting_progress(&initial_sessions);
    let receiver = client.events();
    let app = app.clone();
    let work_run_client = client.clone();
    let schedule_client = client.clone();
    // Toast clicks arrive on a WinRT thread; the listener moves that work onto
    // a thread the Tauri runtime knows about.
    let (toast_activations, toast_clicks) = std::sync::mpsc::channel();
    spawn_toast_activation_listener(app.clone(), toast_clicks);
    let _ = thread::Builder::new()
        .name("terminalai-ui-events".into())
        .spawn(move || {
            let mut waiting = initial_waiting;
            let mut rendered_waiting = None;
            // What each session last said about its own completion, and what
            // that adds up to on the one bar the window has.
            let mut progress = initial_progress;
            // Which sessions already have a toast out. Keyed by id and status so
            // a session that moves from one attention state to another toasts
            // again, while repeated hook deliveries for the same state do not.
            let mut toasted: HashMap<SessionId, SessionStatus> = HashMap::new();
            // A missing Start Menu shortcut makes every toast silently fail.
            // Reported once rather than on every attention event.
            let mut toast_failed = false;
            update_taskbar_waiting_count(&app, waiting.len());
            let mut rendered_progress = fleet_progress(progress.values().copied());
            update_taskbar_progress(&app, rendered_progress);
            let mut pending_logs = VecDeque::<LogEntry>::new();
            let mut next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
            let mut next_log_flush = Instant::now() + LOG_BATCH_INTERVAL;
            let mut next_schedule_check = Instant::now() + SCHEDULE_CHECK_INTERVAL;
            loop {
                let now = Instant::now();
                if now >= next_schedule_check {
                    next_schedule_check = now + SCHEDULE_CHECK_INTERVAL;
                    fire_due_schedule(
                        &schedule_client,
                        &work_schedule_store,
                        &work_run_store,
                        &prompts,
                        SystemTime::now(),
                    );
                }
                let timeout = next_output_flush
                    .saturating_duration_since(now)
                    .min(next_log_flush.saturating_duration_since(now))
                    .min(next_schedule_check.saturating_duration_since(now));
                let received = receiver
                    .lock()
                    .ok()
                    .map(|events| events.recv_timeout(timeout));
                match received {
                    Some(Ok(RegistryEvent::Output { id, data })) => {
                        queue_output(&output_channels, id, data);
                    }
                    Some(Ok(RegistryEvent::Log { entry })) => {
                        pending_logs.push_back(entry);
                        while pending_logs.len() > MAX_LOG_ENTRIES {
                            let _ = pending_logs.pop_front();
                        }
                    }
                    Some(Ok(event)) => {
                        match &event {
                            RegistryEvent::SessionUpdated { session } => {
                                if session.status == SessionStatus::Exited {
                                    if let Err(error) = finish_work_run_session(
                                        &work_run_client,
                                        &work_run_store,
                                        &prompts,
                                        &session.id,
                                    ) {
                                        eprintln!(
                                            "TerminalAI: could not advance the work run after {} exited: {error}",
                                            session.id
                                        );
                                    }
                                }
                                if is_waiting_session(session) {
                                    waiting.insert(session.id.clone());
                                } else {
                                    waiting.remove(&session.id);
                                }
                                match session.task_progress {
                                    Some(reported) => {
                                        progress.insert(session.id.clone(), reported);
                                    }
                                    None => {
                                        progress.remove(&session.id);
                                    }
                                }
                                maybe_toast(
                                    session,
                                    &mut toasted,
                                    &toast_activations,
                                    &mut toast_failed,
                                );
                            }
                            RegistryEvent::SessionRemoved { id } => {
                                if let Err(error) = finish_work_run_session(
                                    &work_run_client,
                                    &work_run_store,
                                    &prompts,
                                    id,
                                ) {
                                    eprintln!(
                                        "TerminalAI: could not advance the work run after {id} was removed: {error}"
                                    );
                                }
                                waiting.remove(id);
                                progress.remove(id);
                                toasted.remove(id);
                            }
                            _ => {}
                        }
                        if rendered_waiting != Some(waiting.len()) {
                            update_taskbar_waiting_count(&app, waiting.len());
                            rendered_waiting = Some(waiting.len());
                        }
                        // Recomputed per event but only sent when it changed:
                        // an agent reporting 40% twice must not cost a window
                        // call, and a chatty fleet emits these continuously.
                        let fleet = fleet_progress(progress.values().copied());
                        if rendered_progress != fleet {
                            update_taskbar_progress(&app, fleet);
                            rendered_progress = fleet;
                        }
                        flush_output_batches(&output_channels);
                        next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                        if !matches!(&event, RegistryEvent::AgentEvent { .. })
                            && app.emit("terminalai:event", event).is_err()
                        {
                            break;
                        }
                    }
                    Some(Err(RecvTimeoutError::Timeout)) => {
                        flush_output_batches(&output_channels);
                        next_output_flush = Instant::now() + OUTPUT_BATCH_INTERVAL;
                    }
                    Some(Err(RecvTimeoutError::Disconnected)) | None => {
                        flush_output_batches(&output_channels);
                        let _ = flush_log_batches(&mut pending_logs, &app);
                        break;
                    }
                }
                if Instant::now() >= next_log_flush {
                    if !flush_log_batches(&mut pending_logs, &app) {
                        break;
                    }
                    next_log_flush = Instant::now() + LOG_BATCH_INTERVAL;
                }
            }
        });
}

/// Raise a toast when a session newly wants the operator.
///
/// Keyed on (id, status): a session moving from `AwaitingInput` to
/// `NeedsApproval` is a new thing to say, but the same status arriving twice —
/// which it does, because hooks fire per tool call — is not.
fn maybe_toast(
    session: &Session,
    toasted: &mut HashMap<SessionId, SessionStatus>,
    activations: &std::sync::mpsc::Sender<toast::ToastActivation>,
    failed: &mut bool,
) {
    if !toast::wants_attention(session.status) {
        // Leaving an attention state clears the memo, so the next one toasts.
        toasted.remove(&session.id);
        return;
    }
    if toasted.get(&session.id) == Some(&session.status) {
        return;
    }
    toasted.insert(session.id.clone(), session.status);
    if let Err(error) =
        toast::raise_attention_toast(APP_USER_MODEL_ID, session, activations.clone())
    {
        if !*failed {
            // Once. A fleet of thirty would otherwise print this per event, and
            // the cause is always the same missing Start Menu shortcut.
            *failed = true;
            eprintln!(
                "terminalai: desktop notifications unavailable ({error}); run the Start-Menu shortcut preflight fix"
            );
        }
    }
}

/// Focus the session a clicked toast names, and raise the window.
fn spawn_toast_activation_listener(
    app: tauri::AppHandle,
    activations: std::sync::mpsc::Receiver<toast::ToastActivation>,
) {
    // `thread::Builder::spawn`, like every other spawn in the workspace: the
    // bare form panics where this returns an error, and a toast listener is not
    // worth taking the window process down for.
    let listener = thread::Builder::new()
        .name("terminalai-toast-activation".into())
        .spawn(move || {
            // The WinRT handler only sends on this channel; everything that touches
            // Tauri happens here, on a thread the runtime knows about.
            while let Ok(toast::ToastActivation::Focus(id)) = activations.recv() {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                let _ = app.emit("terminalai:focus-session", id);
            }
        });
    if let Err(error) = listener {
        // Toasts still fire; clicking one just will not focus its session.
        eprintln!(
            "terminalai: toast activations will not focus their session ({error}); \
             could not start the listener thread"
        );
    }
}

fn is_waiting_session(session: &Session) -> bool {
    matches!(
        session.status,
        SessionStatus::NeedsApproval | SessionStatus::AwaitingInput | SessionStatus::NeedsYou
    )
}

fn waiting_sessions(sessions: &[Session]) -> HashSet<SessionId> {
    sessions
        .iter()
        .filter(|session| is_waiting_session(session))
        .map(|session| session.id.clone())
        .collect()
}

/// Which sessions are reporting how far along they are, keyed so the fleet rule
/// sees them in a stable order.
pub(crate) fn reporting_progress(sessions: &[Session]) -> BTreeMap<SessionId, TaskProgress> {
    sessions
        .iter()
        .filter_map(|session| Some((session.id.clone(), session.task_progress?)))
        .collect()
}

/// Put the fleet's progress on the taskbar, or take the bar away.
///
/// The window has one bar; `fleet_progress` decides what it can honestly say
/// when several agents are reporting. Nothing here invents a value: a fleet
/// where no agent emits `OSC 9;4` shows no bar at all, which is the state the
/// taskbar is in before this ever runs.
fn update_taskbar_progress(app: &tauri::AppHandle, progress: Option<FleetProgress>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let state = match progress {
        None => tauri::window::ProgressBarState {
            status: Some(tauri::window::ProgressBarStatus::None),
            progress: None,
        },
        Some(progress) => tauri::window::ProgressBarState {
            status: Some(match progress.status {
                ProgressStatus::Normal => tauri::window::ProgressBarStatus::Normal,
                ProgressStatus::Error => tauri::window::ProgressBarStatus::Error,
                ProgressStatus::Paused => tauri::window::ProgressBarStatus::Paused,
                ProgressStatus::Indeterminate => tauri::window::ProgressBarStatus::Indeterminate,
            }),
            progress: progress.percent.map(u64::from),
        },
    };
    if let Err(error) = window.set_progress_bar(state) {
        eprintln!("could not update the taskbar progress bar: {error}");
    }
}

#[cfg(target_os = "windows")]
fn taskbar_badge_image(count: usize) -> tauri::image::Image<'static> {
    const SIZE: usize = 32;
    const DIGITS: [[u8; 7]; 10] = [
        [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b11100,
        ],
    ];
    let text = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    let scale = if text.len() == 1 { 3usize } else { 2usize };
    let glyph_width = 5 * scale;
    let spacing = scale;
    let total_width = text.len() * glyph_width + text.len().saturating_sub(1) * spacing;
    let start_x = (SIZE.saturating_sub(total_width)) / 2;
    let start_y = (SIZE - 7 * scale) / 2;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as isize - 16;
            let dy = y as isize - 16;
            if dx * dx + dy * dy <= 15 * 15 {
                let offset = (y * SIZE + x) * 4;
                rgba[offset..offset + 4].copy_from_slice(&[210, 76, 74, 255]);
            }
        }
    }
    let mut cursor_x = start_x;
    for character in text.chars() {
        let glyph = if let Some(digit) = character.to_digit(10) {
            DIGITS[digit as usize]
        } else {
            [
                0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
            ]
        };
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = cursor_x + col * scale + dx;
                        let y = start_y + row * scale + dy;
                        if x < SIZE && y < SIZE {
                            let offset = (y * SIZE + x) * 4;
                            rgba[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
            }
        }
        cursor_x += glyph_width + spacing;
    }
    tauri::image::Image::new_owned(rgba, SIZE as u32, SIZE as u32)
}

fn update_taskbar_waiting_count(app: &tauri::AppHandle, count: usize) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    #[cfg(target_os = "windows")]
    let result = window.set_overlay_icon((count != 0).then(|| taskbar_badge_image(count)));
    #[cfg(not(target_os = "windows"))]
    let result = window.set_badge_count((count != 0).then_some(count as i64));
    if let Err(error) = result {
        eprintln!("could not update taskbar waiting count ({count}): {error}");
    }
}

fn flush_log_batches(pending: &mut VecDeque<LogEntry>, app: &tauri::AppHandle) -> bool {
    if pending.is_empty() {
        return true;
    }
    let batch: Vec<_> = pending.drain(..).collect();
    app.emit("terminalai:logs", batch).is_ok()
}

fn queue_output(output_channels: &OutputChannels, id: SessionId, data: Vec<u8>) {
    let route = output_channels
        .lock()
        .ok()
        .and_then(|channels| channels.get(&id).cloned());
    let Some(route) = route else {
        return;
    };
    if route.queue(data).is_err() {
        remove_output_route(&id, &route, output_channels);
    }
}

fn flush_output_batches(output_channels: &OutputChannels) {
    let routes = output_channels
        .lock()
        .ok()
        .map(|channels| {
            channels
                .iter()
                .map(|(id, route)| (id.clone(), route.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (id, route) in routes {
        if route.flush().is_err() {
            remove_output_route(&id, &route, output_channels);
        }
    }
}
