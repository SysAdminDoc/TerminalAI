use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use terminalai_core::agent::{self, Agent, Origin};
use terminalai_core::land::LandQueue;
use terminalai_core::launch::LaunchSpec;
use terminalai_core::pty::PtySize;
use terminalai_core::{SessionId, SessionRegistry};

use super::http_hooks::HookEndpoint;
use super::persistence;
use super::protocol::{
    admission_settings, ArchiveAfterLanding, FleetSpec, Request, Response, MAX_BROADCAST_TARGETS,
    MAX_HISTORY_BYTES, MAX_WRITE_BYTES,
};

/// Sessions the supervisor did not start, reconciled from the agent's own
/// registry with the CLI as a fallback when that registry is unreadable.
///
/// Never fabricates: an empty result means nothing was discoverable, which the
/// UI renders as "unknown", not as an empty machine.
pub(super) fn external_sessions() -> Vec<terminalai_core::ExternalSession> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    external_sessions_from(&home, || {
        // The registry directory was missing or unreadable. Ask the CLI once.
        agent::resolve(Agent::Claude, None)
            .ok()
            .and_then(|binary| terminalai_core::external::enumerate_via_cli(&binary.path))
            .unwrap_or_default()
    })
}

pub(super) fn external_sessions_from<F>(
    home: &Path,
    fallback: F,
) -> Vec<terminalai_core::ExternalSession>
where
    F: FnOnce() -> Vec<terminalai_core::ExternalSession>,
{
    match terminalai_core::claude_sessions(home, &terminalai_core::external::process_is_running) {
        Some(sessions) => sessions,
        None => fallback(),
    }
}

pub(super) fn dispatch_with_endpoint(
    request: Request,
    registry: &SessionRegistry,
    store_quarantine: Option<&str>,
    hook_endpoint: Option<&HookEndpoint>,
    store_health: Option<&persistence::StoreHealth>,
) -> Response {
    if registry.is_poisoned() && request_requires_registry(&request) {
        return Response::Error {
            message: "registry lock is poisoned; stateful request refused".into(),
        };
    }
    match request {
        Request::Hello { .. } => Response::Error {
            message: "Hello was already completed".into(),
        },
        Request::Subscribe => Response::Error {
            message: "Subscribe is handled by the connection".into(),
        },
        Request::Close => Response::Ok,
        Request::Shutdown => Response::Ok,
        Request::Ping => Response::Pong,
        Request::Snapshot => Response::Snapshot {
            sessions: registry.snapshot(),
            focused: registry.focused(),
            admission: registry.admission_snapshot(),
            store_quarantine: store_quarantine.map(str::to_owned),
            store_write_error: store_health.and_then(|health| {
                health
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            }),
        },
        Request::ReviewSnapshot => Response::ReviewSnapshot {
            entries: registry.review_snapshot(),
        },
        Request::ExternalSessions => Response::ExternalSessions {
            sessions: external_sessions(),
        },
        Request::SessionHistory => Response::SessionHistory {
            archives: registry.archives(),
        },
        Request::StaleWorktrees => Response::StaleWorktrees {
            worktrees: registry.stale_worktrees(),
        },
        Request::ReapWorktree { stale } => match terminalai_core::worktree::reap(&stale) {
            Ok(()) => Response::Ok,
            Err(failures) => Response::Error {
                message: failures.join("; "),
            },
        },
        Request::Land { request } => {
            let outcome = land_queue().land(&request);
            // Only a whole, successful landing does anything further. A refusal
            // -- including the mixed state a failed reversal leaves -- records
            // nothing and archives nothing, because none of it finished.
            let archive = match (&outcome, request.session.as_ref()) {
                (
                    terminalai_core::land::LandOutcome::Landed {
                        files_changed,
                        target_head,
                        verified,
                    },
                    Some(id),
                ) => match owns_source(registry, id, &request.source) {
                    Err(detail) => {
                        // The named row is not the one whose work this was, so
                        // recording the landing on it would file a fact about
                        // one session against another, and archiving it would
                        // retire a row on the strength of unrelated work.
                        // Refuse both, loudly. The work still landed.
                        tracing::warn!(session = %id, %detail, "landed, but the named row does not own the source");
                        request
                            .archive_on_success
                            .then_some(ArchiveAfterLanding::Refused { detail })
                    }
                    Ok(()) => {
                        let landing = terminalai_core::land::Landing {
                            at: SystemTime::now(),
                            target: request.target.clone(),
                            target_head: target_head.clone(),
                            files_changed: *files_changed,
                            verified: *verified,
                        };
                        if let Err(error) = registry.record_landing(id, landing) {
                            // The work landed regardless; a row that vanished
                            // between the two is not a landing failure.
                            tracing::warn!(session = %id, %error, "landed, but the row could not record it");
                        }
                        request
                            .archive_on_success
                            .then(|| match registry.archive(id) {
                                Ok(_) => ArchiveAfterLanding::Archived,
                                Err(error) => ArchiveAfterLanding::Refused {
                                    detail: error.to_string(),
                                },
                            })
                    }
                },
                _ => None,
            };
            Response::Land { outcome, archive }
        }
        Request::AdmissionConfig => Response::Admission {
            admission: admission_settings(&registry.admission_config()),
        },
        Request::SetAdmission {
            max_live_sessions,
            default_budget_usd,
            spend_ceiling_usd,
            spend_window_hours,
            memory_budget_mb,
            session_memory_cap_mb,
            max_processes_per_session,
        } => {
            let config = terminalai_core::registry::AdmissionConfig::new(
                max_live_sessions,
                default_budget_usd,
            )
            .with_spend_ceiling(
                spend_ceiling_usd,
                spend_window_hours
                    .filter(|hours| hours.is_finite() && *hours > 0.0)
                    .map(|hours| Duration::from_secs_f64(hours * 3600.0)),
            )
            .with_memory_limits(
                memory_budget_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
                session_memory_cap_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
                max_processes_per_session,
            );
            registry.set_admission(config);
            Response::Admission {
                admission: admission_settings(&registry.admission_config()),
            }
        }
        Request::MarkReviewed { id } => match registry.mark_reviewed(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Status { id } => match registry
            .snapshot()
            .into_iter()
            .find(|session| session.id == id)
        {
            Some(session) => Response::Status {
                session,
                admission: registry.admission_snapshot(),
            },
            None => Response::Error {
                message: format!("session does not exist: {id}"),
            },
        },
        Request::Resolve {
            agent,
            configured_path,
        } => match agent::resolve(agent, configured_path.as_deref()) {
            Ok(binary) => Response::Resolved {
                agent: binary.agent,
                path: binary.path,
                origin: origin_label(binary.origin).into(),
            },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Capabilities {
            agent,
            configured_path,
        } => match terminalai_core::probe_capabilities(agent, configured_path.as_deref()) {
            Ok(capabilities) => Response::Capabilities { capabilities },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Preview {
            spec,
            configured_path,
        } => {
            let spec = *spec;
            warn_capability_overrides(&spec, configured_path.as_deref());
            match agent::resolve(spec.agent, configured_path.as_deref()) {
                Ok(binary) => match spec.resolve(&binary) {
                    Ok(command) => Response::Preview {
                        command: command.preview(),
                    },
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                },
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::HookEndpoint => match hook_endpoint {
            Some(endpoint) => Response::HookEndpoint {
                endpoint: endpoint.clone(),
            },
            None => Response::Error {
                message: "HTTP hook endpoint is unavailable".into(),
            },
        },
        Request::HookStatus => Response::HookStatus {
            status: registry.hook_delivery_status(),
        },
        Request::Hook { event, hook_token } => {
            let signal = event.signal.clone();
            let matched = registry.apply_hook_matching(
                event,
                hook_token.as_deref(),
                std::time::SystemTime::now(),
            );
            Response::Hook {
                worktree_path: placement_answer(&signal, matched.matched_id(), registry),
                matched: matched.is_matched(),
            }
        }
        Request::AgentEvent { event } => Response::AgentEvent {
            matched: registry.apply_agent_event(event),
        },
        Request::Launch {
            spec,
            configured_path,
        } => {
            let spec = *spec;
            warn_capability_overrides(&spec, configured_path.as_deref());
            match agent::resolve(spec.agent, configured_path.as_deref()) {
                Ok(binary) => match registry.launch(spec, binary) {
                    Ok(id) => match registry.is_queued(&id) {
                        Ok(queued) => Response::Launched { id, queued },
                        Err(error) => Response::Error {
                            message: error.to_string(),
                        },
                    },
                    Err(error) => Response::Error {
                        message: resolve_registry_error(error),
                    },
                },
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::Write { id, data } => {
            // Rejected whole, never truncated: half a prompt reaching an agent is
            // worse than none, because the agent would act on the fragment.
            if data.len() > MAX_WRITE_BYTES {
                Response::Error {
                    message: format!(
                        "write payload of {} bytes exceeds the {MAX_WRITE_BYTES}-byte limit",
                        data.len()
                    ),
                }
            } else {
                match registry.write_user_input(&id, data.as_bytes()) {
                    Ok(()) => Response::Ok,
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                }
            }
        }
        Request::QueuedPrompts { id } => match registry.queued_prompts(&id) {
            Ok(prompts) => Response::QueuedPrompts { prompts },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::EnqueuePrompt { id, text } => {
            if text.len() > MAX_WRITE_BYTES {
                Response::Error {
                    message: format!(
                        "queued prompt of {} bytes exceeds the {MAX_WRITE_BYTES}-byte limit",
                        text.len()
                    ),
                }
            } else {
                match registry.enqueue_prompt(&id, &text) {
                    Ok(prompt) => Response::Enqueued { prompt },
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                }
            }
        }
        Request::EditQueuedPrompt { id, prompt, text } => {
            match registry.edit_queued_prompt(&id, prompt, &text) {
                Ok(()) => Response::Ok,
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::RemoveQueuedPrompt { id, prompt } => {
            match registry.remove_queued_prompt(&id, prompt) {
                Ok(()) => Response::Ok,
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::ReorderQueuedPrompt { id, prompt, to } => {
            match registry.reorder_queued_prompt(&id, prompt, to) {
                Ok(()) => Response::Ok,
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::PauseQueue { id } => match registry.pause_queue(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::ResumeQueue { id } => match registry.resume_queue(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Broadcast { ids, data } => {
            if data.len() > MAX_WRITE_BYTES {
                Response::Error {
                    message: format!(
                        "broadcast payload of {} bytes exceeds the {MAX_WRITE_BYTES}-byte limit",
                        data.len()
                    ),
                }
            } else if ids.len() > MAX_BROADCAST_TARGETS {
                Response::Error {
                    message: format!(
                        "broadcast to {} sessions exceeds the {MAX_BROADCAST_TARGETS}-session limit",
                        ids.len()
                    ),
                }
            } else {
                Response::Broadcast {
                    results: registry.broadcast(&ids, data.as_bytes()),
                }
            }
        }
        Request::Resize {
            id,
            rows,
            cols,
            pixel_width,
            pixel_height,
        } => match registry.resize(
            &id,
            PtySize {
                rows,
                cols,
                pixel_width,
                pixel_height,
            },
        ) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Kill { id } => match registry.kill(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Focus { id } => match registry.focus(id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::MarkRead { id } => match registry.mark_read(&id) {
            Ok(()) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::TogglePin { id } => match registry.toggle_pin(&id) {
            Ok(pinned) => Response::PinChanged { pinned },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::GridSnapshot { id } => match registry.grid_snapshot(&id) {
            Ok(grid) => Response::GridSnapshot { grid },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Scrollback { id } => match registry.scrollback(&id) {
            Ok(data) => Response::Scrollback { data },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::ScrollbackHistory { id, max_bytes } => {
            match registry.scrollback_history(&id, max_bytes.min(MAX_HISTORY_BYTES)) {
                Ok(data) => Response::ScrollbackHistory { data },
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::FleetSpecs => Response::FleetSpecs {
            specs: registry
                .snapshot()
                .into_iter()
                // An exited row's spec is still valid, but a layout describes a
                // working spread — restoring a dozen sessions the operator had
                // already finished with is the opposite of useful.
                .filter(|session| session.status.is_live())
                .filter_map(|session| {
                    registry.spec(&session.id).ok().map(|spec| FleetSpec {
                        id: session.id,
                        pinned: session.pinned,
                        spec: Box::new(spec),
                    })
                })
                .collect(),
        },
        Request::SearchScrollback { query, max_bytes } => {
            // Validated here rather than at the type, because a needle short
            // enough to match everything costs a fleet-wide disk read to say
            // so — and the client can act on being told why.
            match terminalai_core::search::SearchQuery::new(query.needle, query.case_sensitive) {
                Ok(query) => {
                    let searched_bytes = max_bytes.min(MAX_HISTORY_BYTES);
                    Response::SearchResults {
                        matches: registry.search_scrollback(&query, searched_bytes),
                        searched_bytes,
                    }
                }
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Request::Reattach { id } => match registry.reattach(&id) {
            Ok(data) => Response::Reattached { data },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Revive { id } => match registry.revive(&id) {
            Ok(id) => Response::Revived { id },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Request::Archive { id } => match registry.archive(&id) {
            Ok(archive) => Response::Archived { id: archive.id },
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
    }
}

/// Whether `id` is the session whose tree `source` actually is.
///
/// A landing names both a source directory and a session, and nothing forces
/// them to be the same thing. Left unchecked, `--session s0001` against any
/// source at all records a landing on s0001's row and, with the opt-in, retires
/// it — a fact about one session filed against another, and a row archived on
/// the strength of work it never did. Both are silent and neither is easy to
/// notice afterwards, which is why this refuses rather than warns.
///
/// A session's own worktree counts as its source: that is the whole point of
/// giving it one.
pub(super) fn owns_source(
    registry: &SessionRegistry,
    id: &SessionId,
    source: &Path,
) -> Result<(), String> {
    let session = registry
        .snapshot()
        .into_iter()
        .find(|session| session.id == *id)
        .ok_or_else(|| format!("session {id} does not exist"))?;
    // Canonicalised on both sides: these are operator-supplied and daemon-stored
    // paths that routinely disagree on separators and case while naming one
    // directory.
    let same = |candidate: &Path| match (candidate.canonicalize(), source.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        // An unreadable path cannot be shown to match, and this is the side that
        // must fail closed.
        _ => false,
    };
    if same(&session.cwd) {
        return Ok(());
    }
    if let Some(worktree) = session.worktree.as_ref() {
        if same(&worktree.path) {
            return Ok(());
        }
    }
    Err(format!(
        "session {id} did not produce {}; its own tree is {}",
        source.display(),
        session
            .worktree
            .as_ref()
            .map(|worktree| worktree.path.clone())
            .unwrap_or(session.cwd)
            .display()
    ))
}

#[cfg(test)]
pub(super) fn dispatch(request: Request, registry: &SessionRegistry) -> Response {
    dispatch_with_endpoint(request, registry, None, None, None)
}

#[cfg(test)]
pub(super) fn dispatch_with_quarantine(
    request: Request,
    registry: &SessionRegistry,
    store_quarantine: Option<&str>,
) -> Response {
    dispatch_with_endpoint(request, registry, store_quarantine, None, None)
}

/// Which hook, if any, is answered with a worktree path.
///
/// Answered only for the hook that asked. Every other event is fire-and-forget
/// and must stay that way: an adapter that printed a path on an ordinary event
/// would be handing the agent a directive it never requested. An event that
/// matched no row is answered with nothing at all — placement is a fact about a
/// supervised session, and an unauthenticated hook must not learn one.
pub(super) fn placement_answer(
    signal: &terminalai_core::HookSignal,
    matched: Option<&SessionId>,
    registry: &SessionRegistry,
) -> Option<PathBuf> {
    matched
        .filter(|_| matches!(signal, terminalai_core::HookSignal::WorktreeCreate))
        .and_then(|id| registry.worktree_placement(id))
}

pub(super) fn request_requires_registry(request: &Request) -> bool {
    !matches!(
        request,
        Request::Hello { .. }
            | Request::Subscribe
            | Request::Ping
            | Request::Close
            | Request::Shutdown
            | Request::Resolve { .. }
            | Request::Capabilities { .. }
            | Request::Preview { .. }
            | Request::HookEndpoint
            | Request::HookStatus
    )
}

fn warn_capability_overrides(spec: &LaunchSpec, configured_path: Option<&Path>) {
    let Ok(capabilities) = terminalai_core::probe_capabilities(spec.agent, configured_path) else {
        tracing::debug!(agent = ?spec.agent, "runtime capability probe unavailable; launch remains permissive");
        return;
    };
    for warning in capabilities.warnings_for(
        spec.model.as_deref(),
        spec.effort.as_ref(),
        spec.permission.as_ref(),
    ) {
        tracing::warn!(agent = ?spec.agent, warning = %warning, "launch value is outside detected capabilities");
    }
}

fn resolve_registry_error(error: terminalai_core::RegistryError) -> String {
    error.to_string()
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::Configured => "configured",
        Origin::NpmPrefix => "npm-prefix",
        Origin::Path => "path",
    }
}

/// The process-wide landing queue.
///
/// Deliberately a singleton: a per-connection queue would let two clients land
/// at once, which is exactly the interleaving the gate exists to prevent.
pub(super) fn land_queue() -> &'static LandQueue {
    static QUEUE: OnceLock<LandQueue> = OnceLock::new();
    QUEUE.get_or_init(LandQueue::new)
}
