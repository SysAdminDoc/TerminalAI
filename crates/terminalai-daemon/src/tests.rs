use super::*;
use std::fs;
use std::io::{self, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use terminalai_core::AppServerEvent;
use terminalai_core::{
    Agent, AgentEvent, LaunchSpec, ResolvedCommand, Session, SessionId, SessionStoreSnapshot,
    StoredSession, SESSION_STORE_MAGIC, SESSION_STORE_SCHEMA_VERSION,
};

#[test]
fn history_budget_reaches_before_the_memory_ring() {
    assert!(
        MAX_HISTORY_BYTES > terminalai_core::registry::MAX_SCROLLBACK_BYTES as u64,
        "history must include bytes older than the in-memory ring"
    );
    assert!(
        MAX_FRAME_BYTES >= (MAX_HISTORY_BYTES as usize).saturating_mul(4),
        "JSON byte arrays need several times their raw history size"
    );
}

#[test]
fn readable_empty_external_registry_does_not_invoke_cli_fallback() {
    let home = std::env::temp_dir().join(format!(
        "terminalai-daemon-external-empty-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".claude").join("sessions"))
        .expect("create readable empty registry");
    let fallback_called = Arc::new(AtomicBool::new(false));
    let marker = fallback_called.clone();

    let sessions = external_sessions_from(&home, move || {
        marker.store(true, Ordering::Relaxed);
        Vec::new()
    });

    assert!(sessions.is_empty());
    assert!(
        !fallback_called.load(Ordering::Relaxed),
        "a readable empty registry must not spawn the CLI fallback"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn an_endless_frame_is_refused_instead_of_exhausting_memory() {
    // A peer that never sends a newline used to grow a String until the
    // process died — and both ends of this protocol are programs.
    let flood = vec![b'x'; MAX_FRAME_BYTES + 4096];
    let mut reader = BufReader::new(io::Cursor::new(flood));
    let mut line = String::new();
    let error = read_frame(&mut reader, &mut line).expect_err("oversized frame");
    assert!(
        matches!(&error, IpcError::InvalidMessage(message) if message.contains("exceeded")),
        "expected a typed size error, got {error:?}"
    );
}

#[test]
fn a_frame_at_the_limit_still_reads() {
    // The bound must reject what is over it and nothing else.
    let mut body = vec![b'x'; MAX_FRAME_BYTES - 1];
    body.push(b'\n');
    let mut reader = BufReader::new(io::Cursor::new(body));
    let mut line = String::new();
    assert_eq!(
        read_frame(&mut reader, &mut line).expect("frame at the limit"),
        MAX_FRAME_BYTES
    );
}

#[test]
fn frames_are_read_one_at_a_time_and_eof_is_zero() {
    let mut reader = BufReader::new(io::Cursor::new(b"{\"a\":1}\n{\"b\":2}\n".to_vec()));
    let mut line = String::new();
    assert_eq!(read_frame(&mut reader, &mut line).unwrap(), 8);
    assert_eq!(line, "{\"a\":1}\n");
    assert_eq!(read_frame(&mut reader, &mut line).unwrap(), 8);
    assert_eq!(line, "{\"b\":2}\n");
    assert_eq!(read_frame(&mut reader, &mut line).unwrap(), 0);
}

#[test]
fn an_oversized_write_is_refused_whole_and_never_truncated() {
    let registry = SessionRegistry::new();
    let response = dispatch(
        Request::Write {
            id: SessionId::new(1),
            data: "x".repeat(MAX_WRITE_BYTES + 1),
        },
        &registry,
    );
    match response {
        Response::Error { message } => {
            assert!(message.contains("exceeds"), "unhelpful refusal: {message}");
        }
        other => panic!("an oversized write must be refused, got {other:?}"),
    }
    // At the limit it is still the session lookup that decides, not the size.
    let response = dispatch(
        Request::Write {
            id: SessionId::new(1),
            data: "x".repeat(MAX_WRITE_BYTES),
        },
        &registry,
    );
    match response {
        Response::Error { message } => assert!(
            !message.contains("exceeds"),
            "a payload at the limit must not be refused for size: {message}"
        ),
        Response::Ok => {}
        other => panic!("unexpected response {other:?}"),
    }
}

#[test]
fn request_envelope_is_tagged_and_round_trips() {
    let message = WireMessage::Request {
        id: 7,
        request: Request::Ping,
    };
    let json = serde_json::to_string(&message).expect("encode");
    assert!(json.contains("\"kind\":\"request\""));
    assert!(matches!(
        serde_json::from_str(&json),
        Ok(WireMessage::Request { id: 7, .. })
    ));
}

#[test]
fn agent_event_request_is_an_additive_wire_variant() {
    let message = WireMessage::Request {
        id: 8,
        request: Request::AgentEvent {
            event: AgentEvent::AppServer(AppServerEvent::Unknown {
                method: "future/event".into(),
                params: serde_json::json!({"value": true}),
            }),
        },
    };
    let json = serde_json::to_string(&message).expect("encode");
    assert!(json.contains("agent_event"));
    assert!(matches!(
        serde_json::from_str(&json),
        Ok(WireMessage::Request {
            id: 8,
            request: Request::AgentEvent { .. }
        })
    ));
}

#[test]
fn events_are_a_separate_wire_variant() {
    let event = WireMessage::Event {
        event: RegistryEvent::SessionRemoved {
            id: SessionId("s0001".into()),
        },
    };
    let json = serde_json::to_string(&event).expect("encode");
    assert!(json.contains("\"kind\":\"event\""));
    assert!(!json.contains("\"id\":0"));
}

#[test]
fn a_session_owns_its_own_directory_and_its_own_worktree() {
    // The guard must not be so strict it refuses the case it exists to allow:
    // the session that actually did the work.
    let cwd = std::env::temp_dir();
    let spec = terminalai_core::launch::spec_for(terminalai_core::Agent::Claude, &cwd);
    let session = terminalai_core::Session::new(SessionId::new(21), &spec);
    let registry = SessionRegistry::from_store(terminalai_core::store::SessionStoreSnapshot {
        magic: terminalai_core::store::SESSION_STORE_MAGIC.to_owned(),
        schema_version: terminalai_core::store::SESSION_STORE_SCHEMA_VERSION,
        spend: Vec::new(),
        sessions: vec![terminalai_core::store::StoredSession {
            session,
            spec,
            command: terminalai_core::ResolvedCommand {
                program: std::path::PathBuf::from("claude.exe"),
                args: Vec::new(),
                cwd: cwd.clone(),
            },
            scrollback: Vec::new(),
            queue: Default::default(),
        }],
        archives: Vec::new(),
        extra: Default::default(),
    });
    assert!(owns_source(&registry, &SessionId::new(21), &cwd).is_ok());
}

#[test]
fn a_landing_is_not_filed_against_a_session_that_did_not_produce_it() {
    // Found the hard way: naming an unrelated session archived a real row on
    // the strength of a landing it had nothing to do with, and wrote a landing
    // record onto it that was simply false. Both are silent.
    let registry = SessionRegistry::new();
    let elsewhere = std::env::temp_dir();
    assert!(owns_source(&registry, &SessionId("s0001".into()), &elsewhere).is_err());

    let response = dispatch(
        Request::Land {
            request: Box::new(terminalai_core::land::LandRequest {
                source: elsewhere.clone(),
                target: elsewhere,
                session: Some(SessionId("s0001".into())),
                archive_on_success: true,
                expected_target_head: None,
                verify: Vec::new(),
                verify_timeout_secs: None,
            }),
        },
        &registry,
    );
    let Response::Land { archive, .. } = response else {
        panic!("expected a land response");
    };
    // Whatever the landing did, no unrelated row was retired for it.
    assert!(!matches!(archive, Some(ArchiveAfterLanding::Archived)));
}

#[test]
fn a_refused_landing_archives_nothing() {
    // The archive is offered on success only. A landing that refused left the
    // target untouched, so the session still owns work nobody has landed —
    // archiving it there would file unfinished work as finished.
    let missing = std::env::temp_dir().join("terminalai-not-a-repository-at-all");
    let _ = std::fs::remove_dir_all(&missing);
    let response = dispatch(
        Request::Land {
            request: Box::new(terminalai_core::land::LandRequest {
                source: missing.clone(),
                target: missing,
                session: Some(SessionId("s0001".into())),
                // Asked for, and still not done, because the landing failed.
                archive_on_success: true,
                expected_target_head: None,
                verify: Vec::new(),
                verify_timeout_secs: None,
            }),
        },
        &SessionRegistry::new(),
    );
    let Response::Land { outcome, archive } = response else {
        panic!("expected a land response");
    };
    assert!(matches!(
        outcome,
        terminalai_core::land::LandOutcome::Refused(_)
    ));
    assert!(
        archive.is_none(),
        "a refused landing reported an archive result: {archive:?}"
    );
}

#[test]
fn dispatcher_handles_ping_without_a_session() {
    assert!(matches!(
        dispatch(Request::Ping, &SessionRegistry::new()),
        Response::Pong
    ));
}

#[test]
fn snapshot_includes_the_quarantined_store_path() {
    assert!(matches!(
        dispatch_with_quarantine(
            Request::Snapshot,
            &SessionRegistry::new(),
            Some(r"C:\Users\me\sessions.corrupt-2026-08-02T12-34-56Z.json"),
        ),
        Response::Snapshot {
            store_quarantine: Some(path),
            ..
        } if path == r"C:\Users\me\sessions.corrupt-2026-08-02T12-34-56Z.json"
    ));
}

#[test]
fn snapshot_reports_a_store_that_is_not_reaching_disk() {
    // A write failure is silent by nature: the daemon keeps running and every
    // row keeps updating, so without this the operator only finds out by
    // restarting into a fleet that reverted.
    let health: persistence::StoreHealth =
        std::sync::Arc::new(std::sync::Mutex::new(Some("access is denied".into())));
    let response = dispatch_with_endpoint(
        Request::Snapshot,
        &SessionRegistry::new(),
        None,
        None,
        Some(&health),
    );
    assert!(matches!(
        response,
        Response::Snapshot {
            store_write_error: Some(ref error),
            ..
        } if error == "access is denied"
    ));

    // Clearing is as important as reporting: a recovered write must take the
    // banner away rather than leaving the operator told their state is being
    // lost long after it stopped being true.
    *health.lock().expect("health") = None;
    assert!(matches!(
        dispatch_with_endpoint(
            Request::Snapshot,
            &SessionRegistry::new(),
            None,
            None,
            Some(&health),
        ),
        Response::Snapshot {
            store_write_error: None,
            ..
        }
    ));
}

#[test]
fn dispatcher_reports_a_missing_status_without_touching_the_registry() {
    assert!(matches!(
        dispatch(
            Request::Status {
                id: SessionId("missing".into())
            },
            &SessionRegistry::new()
        ),
        Response::Error { message } if message.contains("session does not exist")
    ));
}

#[test]
fn peer_handshake_round_trip_uses_the_named_socket() {
    let name = format!(
        "terminalai-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let server = DaemonServer::bind_named(&name).expect("bind test socket");
    let server_thread = thread::spawn(move || server.serve_one());
    let client = DaemonClient::connect_named(&name).expect("connect test socket");
    let endpoint = client.hook_endpoint().expect("hook endpoint");
    assert!(endpoint.base_url.starts_with("http://127.0.0.1:"));
    assert_eq!(
        endpoint.host,
        endpoint.base_url.trim_start_matches("http://")
    );
    assert_eq!(endpoint.bearer_token.len(), 64);
    assert!(matches!(client.call(Request::Ping), Ok(Response::Pong)));
    assert!(matches!(client.call(Request::Close), Ok(Response::Ok)));
    drop(client);
    server_thread
        .join()
        .expect("server thread")
        .expect("serve client");
}

#[test]
fn shutdown_request_ends_the_accept_loop() {
    let name = format!(
        "terminalai-shutdown-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let server = DaemonServer::bind_named(&name).expect("bind shutdown socket");
    let server_thread = thread::spawn(move || server.serve());
    let client = DaemonClient::connect_named(&name).expect("connect shutdown socket");
    client.shutdown().expect("request daemon shutdown");
    drop(client);
    server_thread
        .join()
        .expect("server thread")
        .expect("serve after shutdown");
}

#[test]
fn shutdown_request_persists_the_final_registry_state_before_server_exits() {
    let dir = std::env::temp_dir().join(format!(
        "terminalai-final-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("final store directory");
    let path = dir.join("sessions.json");
    let cwd = std::env::current_dir().expect("cwd");
    let spec = LaunchSpec {
        agent: Agent::Claude,
        cwd: cwd.clone(),
        ..LaunchSpec::default()
    };
    let id = SessionId::new(1);
    let registry = SessionRegistry::from_store(SessionStoreSnapshot {
        spend: Vec::new(),
        magic: SESSION_STORE_MAGIC.to_owned(),
        schema_version: SESSION_STORE_SCHEMA_VERSION,
        sessions: vec![StoredSession {
            session: Session::new(id.clone(), &spec),
            spec: spec.clone(),
            command: ResolvedCommand {
                program: "claude.exe".into(),
                args: Vec::new(),
                cwd,
            },
            scrollback: Vec::new(),
            queue: Default::default(),
        }],
        archives: Vec::new(),
        extra: Default::default(),
    });
    let writer = StoreWriter::spawn(path.clone(), registry.clone());
    let name = format!(
        "terminalai-final-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let server = DaemonServer::bind_named_with_state(&name, registry, Some(writer), None, None)
        .expect("bind final store socket");
    let server_thread = thread::spawn(move || server.serve());
    let client = DaemonClient::connect_named(&name).expect("connect final store socket");
    client.shutdown().expect("request daemon shutdown");
    drop(client);
    server_thread
        .join()
        .expect("server thread")
        .expect("serve final store");

    let written = SessionStoreSnapshot::read(&path)
        .expect("read final session store")
        .expect("final session store exists");
    assert_eq!(written.sessions.len(), 1);
    assert_eq!(written.sessions[0].session.id, id);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_second_server_cannot_replace_the_first_binding() {
    let name = format!(
        "terminalai-single-instance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let first = DaemonServer::bind_named(&name).expect("first server binds");
    let second = DaemonServer::bind_named(&name);
    assert!(second.is_err(), "second daemon replaced the first binding");
    drop(first);
}

#[test]
fn protocol_skew_names_the_running_daemon_and_stop_action() {
    let message = IpcError::VersionMismatch {
        daemon: 2,
        client: 3,
        daemon_pid: 4242,
    }
    .to_string();
    assert!(message.contains("PID 4242"));
    assert!(message.contains("v2"));
    assert!(message.contains("Stop-Process -Id 4242"));
}

#[cfg(windows)]
#[test]
fn pipe_acl_names_the_current_user_sid() {
    let sddl = current_user_pipe_sddl().expect("current user pipe SDDL");
    assert!(sddl.contains("(A;;GA;;;SY)"));
    assert!(
        sddl.contains("(A;;GA;;;S-"),
        "missing explicit user SID: {sddl}"
    );
    assert!(
        !sddl.contains("OW"),
        "owner-rights alias broadens under elevation: {sddl}"
    );
}

#[cfg(windows)]
fn process_thread_count() -> usize {
    let script = format!(
        "(Get-Process -Id {} -ErrorAction Stop).Threads.Count",
        std::process::id()
    );
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .output()
        .expect("count process threads");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("thread count")
}

#[cfg(windows)]
#[test]
fn subscribed_connections_do_not_leave_event_or_writer_threads() {
    let baseline = process_thread_count();
    for index in 0..100 {
        let name = format!(
            "terminalai-r47-{}-{}-{}",
            std::process::id(),
            index,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let server = DaemonServer::bind_named(&name).expect("bind test socket");
        let server_thread = thread::spawn(move || server.serve_one());
        let client = DaemonClient::connect_named(&name).expect("connect test socket");
        client.subscribe().expect("subscribe test socket");
        assert!(matches!(client.call(Request::Close), Ok(Response::Ok)));
        drop(client);
        server_thread
            .join()
            .expect("server thread")
            .expect("serve client");
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let after = loop {
        let count = process_thread_count();
        if count <= baseline + 2 || std::time::Instant::now() >= deadline {
            break count;
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert!(
        after <= baseline + 2,
        "connection threads leaked: baseline={baseline}, after={after}"
    );
}

/// A registry holding rows restored from a store, which is the only way to put
/// a session in front of the dispatcher without spawning a process.
fn registry_with(sessions: Vec<StoredSession>) -> SessionRegistry {
    SessionRegistry::from_store(SessionStoreSnapshot {
        magic: SESSION_STORE_MAGIC.to_owned(),
        schema_version: SESSION_STORE_SCHEMA_VERSION,
        spend: Vec::new(),
        sessions,
        archives: Vec::new(),
        extra: Default::default(),
    })
}

fn stored_session(id: u64, cwd: &std::path::Path) -> StoredSession {
    let spec = terminalai_core::launch::spec_for(Agent::Claude, cwd);
    StoredSession {
        session: Session::new(SessionId::new(id), &spec),
        spec,
        command: ResolvedCommand {
            program: std::path::PathBuf::from("claude.exe"),
            args: Vec::new(),
            cwd: cwd.to_path_buf(),
        },
        scrollback: Vec::new(),
        queue: Default::default(),
    }
}

#[test]
fn a_poisoned_registry_refuses_state_and_still_answers_the_rest() {
    // The refusal itself is only reachable from here with the core's test-only
    // poison hook: the mutex is private, and leaving this branch unexercised is
    // what the crate's `panic = "abort"` guard exists to prevent. What matters
    // is that the refusal is narrow — a client whose request needs no state
    // must still get an answer, or a poisoned registry takes the whole control
    // plane down with it.
    let registry = SessionRegistry::new();
    registry.poison_state_lock();
    assert!(registry.is_poisoned());

    assert!(matches!(
        dispatch(Request::Snapshot, &registry),
        Response::Error { ref message } if message.contains("poisoned")
    ));
    assert!(matches!(dispatch(Request::Ping, &registry), Response::Pong));
    assert!(matches!(dispatch(Request::Close, &registry), Response::Ok));
    assert!(matches!(
        dispatch(Request::Shutdown, &registry),
        Response::Ok
    ));
    // Handled by the connection before dispatch ever sees them, so reaching
    // this far is itself the error being reported.
    assert!(matches!(
        dispatch(
            Request::Hello {
                protocol: PROTOCOL_VERSION,
                client_pid: 1,
            },
            &registry
        ),
        Response::Error { ref message } if message.contains("already completed")
    ));
    assert!(matches!(
        dispatch(Request::Subscribe, &registry),
        Response::Error { ref message } if message.contains("handled by the connection")
    ));
}

#[test]
fn the_requests_that_survive_a_poisoned_registry_are_the_stateless_ones() {
    // The allowlist decides which requests skip the refusal above. A new
    // stateful variant added to it would reach the registry with a poisoned
    // lock; a stateless one left out of it becomes unanswerable for the rest
    // of the daemon's life.
    for request in [
        Request::Ping,
        Request::Close,
        Request::Shutdown,
        Request::Subscribe,
        Request::HookEndpoint,
        Request::Hello {
            protocol: PROTOCOL_VERSION,
            client_pid: 1,
        },
        Request::Resolve {
            agent: Agent::Claude,
            configured_path: None,
        },
    ] {
        assert!(
            !request_requires_registry(&request),
            "{request:?} must remain answerable while the registry is poisoned"
        );
    }
    for request in [
        Request::Snapshot,
        Request::SessionHistory,
        Request::FleetSpecs,
        Request::AdmissionConfig,
        Request::Focus { id: None },
        Request::Kill {
            id: SessionId::new(1),
        },
        Request::AgentEvent {
            event: AppServerEvent::Unknown {
                method: "thread/unknown".into(),
                params: serde_json::Value::Null,
            }
            .into(),
        },
    ] {
        assert!(
            request_requires_registry(&request),
            "{request:?} touches the registry and must be refused while it is poisoned"
        );
    }
}

#[test]
fn a_hook_is_answered_with_a_path_only_when_it_asked_for_one() {
    // An adapter writes whatever the daemon returns back to the agent, so a
    // path on an ordinary event hands the agent a directive it never asked
    // for. `WorktreeCreate` is the one hook that requested placement, and the
    // fixture is built so that every other signal *would* be answered if the
    // rule were dropped — the row exists, the root is configured, and the
    // placement is derivable.
    let root = std::env::temp_dir().join(format!(
        "terminalai-hook-placement-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    let output = std::process::Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(&repo)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git init failed");
    let repo = repo.canonicalize().expect("canonical repo");

    let id = SessionId::new(31);
    let registry = registry_with(vec![stored_session(31, &repo)]);
    registry.set_worktree_root(root.join("worktrees"));

    let placement = placement_answer(
        &terminalai_core::HookSignal::WorktreeCreate,
        Some(&id),
        &registry,
    )
    .expect("a worktree hook is answered with a path");
    assert!(
        placement.starts_with(root.join("worktrees")),
        "placement escaped the configured root: {}",
        placement.display()
    );

    for signal in [
        terminalai_core::HookSignal::SessionStart,
        terminalai_core::HookSignal::PreToolUse,
        terminalai_core::HookSignal::CwdChanged,
        terminalai_core::HookSignal::Stop,
        terminalai_core::HookSignal::Unknown {
            event: "SomethingNew".into(),
        },
    ] {
        assert_eq!(
            placement_answer(&signal, Some(&id), &registry),
            None,
            "{signal:?} was handed a path it never asked for"
        );
    }

    // A hook that matched no row is told nothing, even though it asked: where a
    // checkout would be placed is a fact about a supervised session, and an
    // unauthenticated event must not learn one.
    assert_eq!(
        placement_answer(
            &terminalai_core::HookSignal::WorktreeCreate,
            None,
            &registry
        ),
        None,
        "an unmatched hook was answered with a placement"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn oversized_queued_prompts_and_broadcasts_are_refused_whole() {
    // Same reasoning as the write cap: half a prompt reaching an agent is
    // worse than none, because the agent acts on the fragment.
    let registry = SessionRegistry::new();
    let too_long = "x".repeat(MAX_WRITE_BYTES + 1);
    assert!(matches!(
        dispatch(
            Request::EnqueuePrompt {
                id: SessionId::new(1),
                text: too_long.clone(),
            },
            &registry
        ),
        Response::Error { ref message } if message.contains("exceeds the")
    ));
    assert!(matches!(
        dispatch(
            Request::Broadcast {
                ids: vec![SessionId::new(1)],
                data: too_long,
            },
            &registry
        ),
        Response::Error { ref message } if message.contains("exceeds the")
    ));
    // The second broadcast limit is the fan-out, not the payload: one small
    // prompt aimed at every id a client cares to name is a fleet-wide write.
    let response = dispatch(
        Request::Broadcast {
            ids: (0..=MAX_BROADCAST_TARGETS as u64)
                .map(SessionId::new)
                .collect(),
            data: "hello".into(),
        },
        &registry,
    );
    assert!(matches!(
        response,
        Response::Error { ref message } if message.contains("exceeds the")
    ));
    // And a broadcast inside both limits is answered per session rather than
    // with one verdict for the whole fan-out.
    let response = dispatch(
        Request::Broadcast {
            ids: vec![SessionId::new(1), SessionId::new(2)],
            data: "hello".into(),
        },
        &registry,
    );
    let Response::Broadcast { results } = response else {
        panic!("expected a broadcast response");
    };
    assert_eq!(results.len(), 2);
}

#[test]
fn a_search_reports_the_byte_ceiling_it_actually_used() {
    // Clamped rather than refused, and the clamp is reported: a client that
    // asked for more than a frame can carry otherwise reads a partial search
    // as an exhaustive one.
    let response = dispatch(
        Request::SearchScrollback {
            query: terminalai_core::search::SearchQuery {
                needle: "needle".into(),
                case_sensitive: false,
            },
            max_bytes: u64::MAX,
        },
        &SessionRegistry::new(),
    );
    assert!(matches!(
        response,
        Response::SearchResults {
            searched_bytes,
            ..
        } if searched_bytes == MAX_HISTORY_BYTES
    ));

    // A needle short enough to match everything costs a fleet-wide disk read to
    // say so, so it is refused with a reason the client can act on.
    assert!(matches!(
        dispatch(
            Request::SearchScrollback {
                query: terminalai_core::search::SearchQuery {
                    needle: String::new(),
                    case_sensitive: false,
                },
                max_bytes: 1024,
            },
            &SessionRegistry::new()
        ),
        Response::Error { .. }
    ));
}

#[test]
fn a_saved_layout_describes_live_rows_only() {
    // A restored fleet is not running. Its specs are still valid, but a layout
    // describes a working spread, and restoring a dozen sessions the operator
    // had already finished with is the opposite of useful.
    let cwd = std::env::temp_dir();
    let registry = registry_with(vec![stored_session(41, &cwd), stored_session(42, &cwd)]);
    let Response::Snapshot { sessions, .. } = dispatch(Request::Snapshot, &registry) else {
        panic!("expected a snapshot");
    };
    assert_eq!(sessions.len(), 2, "the rows themselves are still tracked");
    assert!(
        sessions.iter().all(|session| !session.status.is_live()),
        "a restored row must not report as live"
    );

    let Response::FleetSpecs { specs } = dispatch(Request::FleetSpecs, &registry) else {
        panic!("expected fleet specs");
    };
    assert!(
        specs.is_empty(),
        "a layout captured rows that are not running: {specs:?}"
    );
}

#[test]
fn admission_is_echoed_back_in_the_units_it_was_set_in() {
    // The dialog sends megabytes and hours; the core stores bytes and a
    // duration. Both conversions happen here, and a round trip is the only
    // place the pair can be shown to agree.
    let registry = SessionRegistry::new();
    let response = dispatch(
        Request::SetAdmission {
            max_live_sessions: 7,
            default_budget_usd: Some(2.5),
            spend_ceiling_usd: Some(40.0),
            spend_window_hours: Some(6.0),
            memory_budget_mb: Some(8192),
            session_memory_cap_mb: Some(512),
            max_processes_per_session: Some(24),
        },
        &registry,
    );
    let Response::Admission { admission } = response else {
        panic!("expected an admission response");
    };
    assert_eq!(admission.max_live_sessions, 7);
    assert_eq!(admission.memory_budget_mb, Some(8192));
    assert_eq!(admission.session_memory_cap_mb, Some(512));
    assert_eq!(admission.max_processes_per_session, Some(24));
    assert!((admission.spend_window_hours - 6.0).abs() < f64::EPSILON);
    // Stored, not merely echoed: reading it back through a separate request is
    // what distinguishes applying the policy from reporting the argument.
    let Response::Admission { admission } = dispatch(Request::AdmissionConfig, &registry) else {
        panic!("expected an admission response");
    };
    assert_eq!(admission.max_live_sessions, 7);
    assert_eq!(admission.spend_ceiling_usd, Some(40.0));

    // A window of zero or a NaN is not a window. Dropping it leaves the
    // configured default standing rather than a ceiling measured over no time
    // at all, which would refuse every launch.
    for hours in [Some(0.0), Some(f64::NAN), Some(-1.0)] {
        let Response::Admission { admission } = dispatch(
            Request::SetAdmission {
                max_live_sessions: 7,
                default_budget_usd: None,
                spend_ceiling_usd: Some(40.0),
                spend_window_hours: hours,
                memory_budget_mb: None,
                session_memory_cap_mb: None,
                max_processes_per_session: None,
            },
            &registry,
        ) else {
            panic!("expected an admission response");
        };
        assert!(
            admission.spend_window_hours > 0.0,
            "a meaningless window ({hours:?}) became the live one"
        );
    }
}

#[test]
fn a_landing_names_the_tree_the_session_actually_holds() {
    // The refusal has to say which directory the row owns, because the
    // operator's next move is to re-run the landing against that one.
    let cwd = std::env::temp_dir().join("terminalai-owns-source");
    fs::create_dir_all(&cwd).expect("session cwd");
    let registry = registry_with(vec![stored_session(51, &cwd)]);
    let elsewhere = std::env::temp_dir().join("terminalai-not-this-session");
    fs::create_dir_all(&elsewhere).expect("other dir");

    let detail = owns_source(&registry, &SessionId::new(51), &elsewhere)
        .expect_err("an unrelated directory must not pass the ownership check");
    assert!(
        detail.contains(&cwd.display().to_string()),
        "the refusal did not name the session's own tree: {detail}"
    );

    let _ = fs::remove_dir_all(&elsewhere);
}

#[test]
fn every_request_that_names_a_session_refuses_one_that_is_not_there() {
    // A request answered with `Ok` for a row that does not exist tells the
    // window the action worked. The operator sees no error, the fleet does not
    // change, and the next thing they do is repeat it. Each of these is a
    // separate arm, so the guarantee has to be checked arm by arm.
    let registry = SessionRegistry::new();
    let id = || SessionId::new(404);
    let requests = vec![
        Request::MarkReviewed { id: id() },
        Request::MarkRead { id: id() },
        Request::TogglePin { id: id() },
        Request::Kill { id: id() },
        Request::Focus { id: Some(id()) },
        Request::Write {
            id: id(),
            data: "hello".into(),
        },
        Request::Resize {
            id: id(),
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        Request::QueuedPrompts { id: id() },
        Request::EnqueuePrompt {
            id: id(),
            text: "hello".into(),
        },
        Request::EditQueuedPrompt {
            id: id(),
            prompt: 1,
            text: "hello".into(),
        },
        Request::RemoveQueuedPrompt {
            id: id(),
            prompt: 1,
        },
        Request::ReorderQueuedPrompt {
            id: id(),
            prompt: 1,
            to: 0,
        },
        Request::PauseQueue { id: id() },
        Request::ResumeQueue { id: id() },
        Request::Scrollback { id: id() },
        Request::ScrollbackHistory {
            id: id(),
            max_bytes: 1024,
        },
        Request::GridSnapshot { id: id() },
        Request::Reattach { id: id() },
        Request::Revive { id: id() },
        Request::Archive { id: id() },
        Request::Status { id: id() },
    ];
    for request in requests {
        let label = format!("{request:?}");
        let response = dispatch(request, &registry);
        let Response::Error { message } = response else {
            panic!("{label} was answered with {response:?} for a session that does not exist");
        };
        assert!(
            message.contains("404"),
            "{label} refused without naming the session: {message}"
        );
    }
}

#[test]
fn each_read_only_request_is_answered_in_its_own_shape() {
    // These arms are one-line delegations, which is exactly why they are worth
    // pinning: a client matches on the response kind, and an arm wired to the
    // neighbouring reader answers plausibly and wrongly.
    let registry = SessionRegistry::new();
    assert!(matches!(
        dispatch(Request::ReviewSnapshot, &registry),
        Response::ReviewSnapshot { .. }
    ));
    assert!(matches!(
        dispatch(Request::SessionHistory, &registry),
        Response::SessionHistory { .. }
    ));
    assert!(matches!(
        dispatch(Request::StaleWorktrees, &registry),
        Response::StaleWorktrees { .. }
    ));
    assert!(matches!(
        dispatch(Request::ExternalSessions, &registry),
        Response::ExternalSessions { .. }
    ));
    assert!(matches!(
        dispatch(Request::FleetSpecs, &registry),
        Response::FleetSpecs { .. }
    ));
    assert!(matches!(
        dispatch(Request::AdmissionConfig, &registry),
        Response::Admission { .. }
    ));
}

#[test]
fn the_hook_endpoint_says_it_is_unavailable_rather_than_inventing_one() {
    // The HTTP listener is optional. A client that got silence here would wait;
    // a client told the endpoint is absent falls back to the pipe.
    assert!(matches!(
        dispatch(Request::HookEndpoint, &SessionRegistry::new()),
        Response::Error { ref message } if message.contains("unavailable")
    ));
}
