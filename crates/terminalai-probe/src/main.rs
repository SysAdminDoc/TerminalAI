//! Headless harness for the parts of TerminalAI that touch the machine.
//!
//! The GUI cannot be unit-tested against a real agent process, so this binary
//! carries that burden: it resolves the executables, prints the exact argument
//! vector a launcher choice produces, and drives a real ConPTY end to end.
//!
//!   terminalai-probe resolve
//!   terminalai-probe capabilities codex --json
//!   terminalai-probe preview claude --model opus --effort xhigh --cwd .
//!   terminalai-probe spawn   codex  --raw --version
//!
//! Exit codes: 0 success, 1 usage error, 2 resolution failure, 3 spawn failure.

use std::path::PathBuf;
// Spawning a child directly is the Windows-only `exec` path; the control-plane
// commands go through the daemon.
#[cfg(windows)]
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use std::{io, io::BufRead, io::Read, io::Write};

use serde::Serialize;
use terminalai_core::agent::{self, Agent, AgentBinary};
use terminalai_core::compatibility::{
    CompatibilityFixture, CompatibilityStatus, MATRIX_SCHEMA_VERSION,
};
use terminalai_core::launch::{Effort, LaunchSpec, Permission, ResolvedCommand, Sandbox};
use terminalai_core::pty::{self, PtySession};
use terminalai_core::{parse_hook_in, HookTransport, SessionId};
use terminalai_daemon::{DaemonClient, HookEndpoint, Request, Response};

const HEADER: &str = "\
terminalai-probe — machine-facing checks for TerminalAI

USAGE:
";

const OPTIONS: &str = "\
OPTIONS:
  --cwd <dir>          working directory (default: current)
  --model <name>
  --effort <runtime value>
  --permission <ask|plan|accept-edits|bypass>
  --sandbox <read-only|workspace-write|danger-full-access>   (codex only)
  --name <label>                                             (claude only)
  --prompt <text>
  --worktree                                                 (private Git checkout + branch)
  --agent-home <dir>                                         (CLAUDE_CONFIG_DIR / CODEX_HOME)
  --env-passthrough <NAME>                                   (inherit one parent variable; repeatable)
  --setup-hook <command>                                     (optional per-session setup)
  --teardown-hook <command>                                  (optional per-session teardown)
  --port-base <1024..65535>                                  (default 42000)
  --port-count <0..16>                                       (default 4)
  --timeout <secs>     spawn only; default 30
  --raw <arg>...       everything after this is passed through verbatim

HOOK OPTIONS:
  --config <path>      override the agent settings file
  --executable <path>  executable used by the installed hook command
";

/// One subcommand: the word that selects it, the help line that describes
/// it, and the function it runs.
///
/// The three travel together deliberately. `USAGE` used to be a hand-written
/// constant beside a `match`, and the two disagreed: the binary dispatched
/// twenty-nine subcommands and advertised twenty-six, so `auth`, `exec` and
/// `limits` were reachable and undocumented. A table cannot drift from
/// itself -- adding an arm without a help line does not compile.
struct Subcommand {
    name: &'static str,
    /// The `USAGE` line, verbatim, including any wrapped continuation.
    synopsis: &'static str,
    run: fn(&[String]) -> i32,
}

const COMMANDS: &[Subcommand] = &[
    Subcommand {
        name: "resolve",
        synopsis: "terminalai-probe resolve",
        run: |_| cmd_resolve(),
    },
    Subcommand {
        name: "auth",
        synopsis: "terminalai-probe auth                                    (are both agents signed in?)",
        run: |_| cmd_auth(),
    },
    Subcommand {
        name: "capabilities",
        synopsis: "terminalai-probe capabilities <claude|codex> [--json]",
        run: |args| cmd_capabilities(&args[1..]),
    },
    Subcommand {
        name: "preview",
        synopsis: "terminalai-probe preview <claude|codex> [options]",
        run: |args| cmd_build(&args[1..], false),
    },
    Subcommand {
        name: "spawn",
        synopsis: "terminalai-probe spawn   <claude|codex> [options] [--raw <arg>...]",
        run: |args| cmd_build(&args[1..], true),
    },
    Subcommand {
        name: "list",
        synopsis: "terminalai-probe list    --json",
        run: |args| cmd_list(&args[1..]),
    },
    Subcommand {
        name: "start",
        synopsis: "terminalai-probe start   <claude|codex> [options] --json",
        run: |args| cmd_start(&args[1..]),
    },
    Subcommand {
        name: "stop",
        synopsis: "terminalai-probe stop    <session-id> --json",
        run: |args| cmd_stop(&args[1..]),
    },
    Subcommand {
        name: "send",
        synopsis: "terminalai-probe send    <session-id> <text> --json",
        run: |args| cmd_send(&args[1..]),
    },
    Subcommand {
        name: "broadcast",
        synopsis: "terminalai-probe broadcast <session-id>... -- <text> [--json]  (one prompt, many sessions)",
        run: |args| cmd_broadcast(&args[1..]),
    },
    Subcommand {
        name: "queue",
        synopsis: "terminalai-probe queue   <session-id> [add <text>|pause|resume] [--json]",
        run: |args| cmd_queue(&args[1..]),
    },
    Subcommand {
        name: "status",
        synopsis: "terminalai-probe status  <session-id> --json",
        run: |args| cmd_status(&args[1..]),
    },
    Subcommand {
        name: "limits",
        synopsis: "terminalai-probe limits  [--max-live <n>] [--json]        (read or set the fleet limits)",
        run: |args| cmd_limits(&args[1..]),
    },
    Subcommand {
        name: "shutdown",
        synopsis: "terminalai-probe shutdown",
        run: |args| cmd_shutdown(&args[1..]),
    },
    Subcommand {
        name: "exec",
        synopsis: "terminalai-probe exec    <program> [args...]              (any command on a pseudo-console)",
        run: |args| cmd_exec(&args[1..]),
    },
    Subcommand {
        name: "cpu-idle",
        synopsis: "terminalai-probe cpu-idle [--sessions <n>] [--seconds <s>] [--poll]",
        run: |args| cmd_cpu_idle(&args[1..]),
    },
    Subcommand {
        name: "fleet-stress",
        synopsis: "terminalai-probe fleet-stress [--sessions <n>] [--events-per-session <n>] [--json] [--output <path>]",
        run: |args| cmd_fleet_stress(&args[1..]),
    },
    Subcommand {
        name: "hygiene",
        synopsis: "terminalai-probe hygiene  [--sessions <n>] [--json] [--output <path>]",
        run: |args| cmd_hygiene(&args[1..]),
    },
    Subcommand {
        name: "land",
        synopsis: "terminalai-probe land    --source <dir> --target <dir> [--expect-head <sha>]
                           [--verify <program> [--verify-arg <arg>]...] [--verify-timeout <s>]
                           [--session <id> [--archive-on-success]]",
        run: |args| cmd_land(&args[1..]),
    },
    Subcommand {
        name: "mcp",
        synopsis: "terminalai-probe mcp     [--write-token <t> --write-session <id>]...  (MCP server on stdio)",
        run: |args| cmd_mcp(&args[1..]),
    },
    Subcommand {
        name: "pin",
        synopsis: "terminalai-probe pin     <session-id> --json      (toggle a pinned live grid)",
        run: |args| cmd_pin(&args[1..]),
    },
    Subcommand {
        name: "grid",
        synopsis: "terminalai-probe grid    <session-id> --json      (parsed grid for a pinned pane)",
        run: |args| cmd_grid(&args[1..]),
    },
    Subcommand {
        name: "history",
        synopsis: "terminalai-probe history <session-id> [bytes] [--json]  (output the memory ring has dropped)",
        run: |args| cmd_history(&args[1..]),
    },
    Subcommand {
        name: "search",
        synopsis: "terminalai-probe search <needle> [--case] [--json]   (find a string across every session)",
        run: |args| cmd_search(&args[1..]),
    },
    Subcommand {
        name: "archives",
        synopsis: "terminalai-probe archives --json                   (sessions this supervisor finished)",
        run: |args| cmd_archives(&args[1..]),
    },
    Subcommand {
        name: "archive",
        synopsis: "terminalai-probe archive <session-id> [--json]     (retire a stopped row into the history)",
        run: |args| cmd_archive(&args[1..]),
    },
    Subcommand {
        name: "worktrees",
        synopsis: "terminalai-probe worktrees [--json]                (checkouts no live session owns)",
        run: |args| cmd_worktrees(&args[1..]),
    },
    Subcommand {
        name: "hook",
        synopsis: "terminalai-probe hook    <claude|codex>    (read one hook JSON object from stdin)",
        run: |args| cmd_hook(&args[1..]),
    },
    Subcommand {
        name: "hooks",
        synopsis: "terminalai-probe hooks   <status|preview|install|remove> <claude|codex> [options]",
        run: |args| cmd_hooks(&args[1..]),
    },
    Subcommand {
        name: "verify-goldens",
        synopsis: "terminalai-probe verify-goldens [--goldens <dir>]  (verify versioned argv/capability matrix against installed CLIs)",
        run: |args| cmd_verify_goldens(&args[1..]),
    },
];

/// The help text, built from the dispatch table.
fn usage() -> String {
    let mut text = String::from(HEADER);
    for command in COMMANDS {
        text.push_str("  ");
        text.push_str(command.synopsis);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(OPTIONS);
    text
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--help") | Some("-h") | None => {
            print!("{}", usage());
            0
        }
        Some(name) => match COMMANDS.iter().find(|command| command.name == name) {
            Some(command) => (command.run)(&args),
            None => {
                eprintln!("unknown command: {name}\n\n{}", usage());
                1
            }
        },
    };
    std::process::exit(code);
}

fn cmd_capabilities(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let Some(agent_arg) = args.first() else {
        return control_usage("capabilities <claude|codex> [--json]");
    };
    if args.len() != 1 {
        return control_usage("capabilities accepts one agent and optional --json");
    }
    let agent = match agent_arg.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => return control_usage(&format!("unknown agent: {other}")),
    };
    match terminalai_core::probe_capabilities(agent, None) {
        Ok(capabilities) if machine => match serde_json::to_string(&capabilities) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(error) => print_control_error(error, true),
        },
        Ok(capabilities) => {
            println!("{capabilities:#?}");
            0
        }
        Err(error) => print_control_error(error, machine),
    }
}

fn cmd_list(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    if !args.is_empty() {
        return control_usage("list takes no arguments other than --json");
    }
    match control_call(Request::Snapshot) {
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

fn cmd_start(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let (spec, _) = match parse_launch_spec(&args, false) {
        Ok(parsed) => parsed,
        Err(error) => return control_usage(&error),
    };
    match control_call(Request::Launch {
        spec: Box::new(spec),
        configured_path: None,
    }) {
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

fn cmd_stop(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let id = match one_control_argument(&args, "stop <session-id> [--json]") {
        Ok(id) => terminalai_core::SessionId(id),
        Err(error) => return control_usage(&error),
    };
    match control_call(Request::Kill { id }) {
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

fn cmd_send(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    if args.len() < 2 {
        return control_usage("send <session-id> <text> [--json]");
    }
    let id = terminalai_core::SessionId(args[0].clone());
    let text = args[1..].join(" ");
    let data = bracketed_paste(&text);
    match control_call(Request::Write { id, data }) {
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Send one prompt to several sessions.
///
/// Exits non-zero if any target refused, so a script broadcasting to a fleet
/// finds out rather than reading "ok" and assuming every agent got it.
fn cmd_broadcast(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        return control_usage("broadcast <session-id>... -- <text> [--json]");
    };
    if separator == 0 || separator + 1 >= args.len() {
        return control_usage("broadcast <session-id>... -- <text> [--json]");
    }
    let ids: Vec<SessionId> = args[..separator]
        .iter()
        .map(|id| SessionId(id.clone()))
        .collect();
    let data = bracketed_paste(&args[separator + 1..].join(" "));
    match control_call(Request::Broadcast { ids, data }) {
        Ok(Response::Broadcast { results }) if machine => print_json(results),
        Ok(Response::Broadcast { results }) => {
            let mut refused = 0;
            for result in &results {
                match &result.refusal {
                    None => println!("{} delivered", result.id.0),
                    Some(refusal) => {
                        refused += 1;
                        println!("{} skipped: {refusal}", result.id.0);
                    }
                }
            }
            i32::from(refused > 0)
        }
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Inspect or drive one session's prompt queue.
fn cmd_queue(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let Some(id) = args.first().map(|id| SessionId(id.clone())) else {
        return control_usage("queue <session-id> [add <text> | pause | resume] [--json]");
    };
    let request = match args.get(1).map(String::as_str) {
        None | Some("list") => Request::QueuedPrompts { id },
        Some("pause") => Request::PauseQueue { id },
        Some("resume") => Request::ResumeQueue { id },
        Some("add") if args.len() > 2 => Request::EnqueuePrompt {
            id,
            text: args[2..].join(" "),
        },
        _ => return control_usage("queue <session-id> [add <text> | pause | resume] [--json]"),
    };
    match control_call(request) {
        Ok(Response::QueuedPrompts { prompts }) if machine => print_json(prompts),
        Ok(Response::QueuedPrompts { prompts }) => {
            if prompts.is_empty() {
                println!("nothing queued");
            }
            for (index, prompt) in prompts.iter().enumerate() {
                println!("{}. [{}] {}", index + 1, prompt.id, prompt.text);
            }
            0
        }
        Ok(Response::Enqueued { prompt }) if machine => print_json(serde_json::json!({ "prompt": prompt })),
        Ok(Response::Enqueued { prompt }) => {
            println!("queued as {prompt}");
            0
        }
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Read or set the daemon-wide admission policy over the control pipe.
///
/// The dialog cannot be unit-tested against a live daemon, so this drives the
/// same two requests it does: `limits` reads, `limits --max-live N` writes.
fn cmd_limits(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let mut max_live: Option<usize> = None;
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        if argument == "--max-live" {
            match iter.next().and_then(|value| value.parse::<usize>().ok()) {
                Some(value) => max_live = Some(value),
                None => return control_usage("--max-live needs a positive integer"),
            }
        }
    }
    let request = match max_live {
        None => Request::AdmissionConfig,
        Some(max_live_sessions) => {
            let current = match control_call(Request::AdmissionConfig) {
                Ok(Response::Admission { admission }) => admission,
                Ok(other) => return print_control_response(other, machine),
                Err(error) => return print_control_error(error, machine),
            };
            Request::SetAdmission {
                max_live_sessions,
                default_budget_usd: current.default_budget_usd,
                spend_ceiling_usd: current.spend_ceiling_usd,
                spend_window_hours: Some(current.spend_window_hours),
                memory_budget_mb: current.memory_budget_mb,
                session_memory_cap_mb: current.session_memory_cap_mb,
                max_processes_per_session: current.max_processes_per_session,
            }
        }
    };
    match control_call(request) {
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

fn cmd_status(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let id = match one_control_argument(&args, "status <session-id> [--json]") {
        Ok(id) => terminalai_core::SessionId(id),
        Err(error) => return control_usage(&error),
    };
    match control_call(Request::Status { id }) {
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Exercise the land gate through the daemon.
///
/// The gate's own tests call the module directly; this drives the same request
/// over the control pipe, so the daemon wiring is covered by something other
/// than inspection.
fn cmd_land(args: &[String]) -> i32 {
    let mut source: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut expect_head: Option<String> = None;
    let mut verify: Vec<String> = Vec::new();
    let mut verify_timeout: Option<u64> = None;
    let mut session: Option<String> = None;
    let mut archive_on_success = false;
    let mut index = 0;
    while index < args.len() {
        let take = |index: &mut usize, flag: &str| -> Option<String> {
            *index += 1;
            args.get(*index).cloned().or_else(|| {
                eprintln!("{flag} needs a value");
                None
            })
        };
        match args[index].as_str() {
            "--source" => match take(&mut index, "--source") {
                Some(value) => source = Some(PathBuf::from(value)),
                None => return 2,
            },
            "--target" => match take(&mut index, "--target") {
                Some(value) => target = Some(PathBuf::from(value)),
                None => return 2,
            },
            "--expect-head" => match take(&mut index, "--expect-head") {
                Some(value) => expect_head = Some(value),
                None => return 2,
            },
            "--verify" => match take(&mut index, "--verify") {
                Some(value) => verify.push(value),
                None => return 2,
            },
            "--verify-arg" => match take(&mut index, "--verify-arg") {
                Some(value) => verify.push(value),
                None => return 2,
            },
            "--verify-timeout" => match take(&mut index, "--verify-timeout") {
                Some(value) => match value.parse() {
                    Ok(seconds) => verify_timeout = Some(seconds),
                    Err(_) => {
                        eprintln!("--verify-timeout needs whole seconds");
                        return 2;
                    }
                },
                None => return 2,
            },
            "--session" => match take(&mut index, "--session") {
                Some(value) => session = Some(value),
                None => return 2,
            },
            // Opt-in, and only meaningful with --session: there is nothing to
            // archive when the landing is not a session's.
            "--archive-on-success" => archive_on_success = true,
            "--json" => {}
            other => {
                eprintln!("unknown land option: {other}");
                return 2;
            }
        }
        index += 1;
    }
    let (Some(source), Some(target)) = (source, target) else {
        return control_usage("land needs --source and --target");
    };
    if archive_on_success && session.is_none() {
        return control_usage("--archive-on-success needs --session; there is no row to archive");
    }
    let request = terminalai_core::land::LandRequest {
        source,
        target,
        session: session.map(terminalai_core::SessionId),
        archive_on_success,
        expected_target_head: expect_head,
        verify,
        verify_timeout_secs: verify_timeout,
    };
    match control_call(Request::Land {
        request: Box::new(request),
    }) {
        Ok(Response::Land { outcome, archive }) => {
            let refused = matches!(outcome, terminalai_core::land::LandOutcome::Refused(_));
            // One object, both answers. A caller that asked to archive needs to
            // see whether it happened without a second round trip.
            print_json(serde_json::json!({ "land": outcome, "archive": archive }));
            // A refusal is a normal, expected answer, but it is not a success:
            // a script that lands in a loop must be able to tell them apart.
            i32::from(refused)
        }
        Ok(response) => print_control_response(response, true),
        Err(error) => print_control_error(error, true),
    }
}

/// Bridge the fleet to an MCP client over stdio.
///
/// The daemon owns the registry, so this is a thin translator: one JSON-RPC
/// line in, one control request out, one line back. Every fleet read is a fresh
/// daemon call rather than a cached snapshot, because a sibling agent asking
/// "what is s0002 doing" wants the answer now.
fn cmd_mcp(args: &[String]) -> i32 {
    let mut token: Option<String> = None;
    let mut sessions: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--write-token" => {
                index += 1;
                match args.get(index) {
                    Some(value) => token = Some(value.clone()),
                    None => {
                        eprintln!("--write-token needs a value");
                        return 2;
                    }
                }
            }
            "--write-session" => {
                index += 1;
                match args.get(index) {
                    Some(value) => sessions.push(value.clone()),
                    None => {
                        eprintln!("--write-session needs a session id");
                        return 2;
                    }
                }
            }
            other => {
                eprintln!("unknown mcp option: {other}");
                return 2;
            }
        }
        index += 1;
    }

    // Both halves or neither. A token with no opted-in session would advertise
    // mutating tools that always refuse, and an opted-in session with no token
    // reads as protection that is not there.
    if token.is_some() != !sessions.is_empty() {
        eprintln!(
            "--write-token and --write-session must be given together; without both the server is read-only"
        );
        return 2;
    }
    let gate = match token {
        Some(token) => terminalai_core::mcp::WriteGate::new(token, sessions),
        None => terminalai_core::mcp::WriteGate::default(),
    };

    let mut server = terminalai_core::mcp::McpServer::new(DaemonFleet, gate);
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("mcp: could not read stdin: {error}");
                return 1;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle_line(&line) {
            if writeln!(stdout, "{response}").is_err() || stdout.flush().is_err() {
                // The client closed the pipe. Not an error worth a nonzero exit.
                return 0;
            }
        }
    }
    0
}

/// [`FleetAccess`] backed by the running daemon.
struct DaemonFleet;

impl DaemonFleet {
    fn call(&self, request: Request) -> Result<Response, String> {
        control_call(request)
    }
}

impl terminalai_core::mcp::FleetAccess for DaemonFleet {
    fn sessions(&self) -> Result<Vec<serde_json::Value>, String> {
        match self.call(Request::Snapshot)? {
            Response::Snapshot { sessions, .. } => sessions
                .into_iter()
                .map(|session| {
                    serde_json::to_value(session).map_err(|error| error.to_string())
                })
                .collect(),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected snapshot response: {other:?}")),
        }
    }

    fn external_sessions(&self) -> Result<Vec<serde_json::Value>, String> {
        match self.call(Request::ExternalSessions)? {
            Response::ExternalSessions { sessions } => sessions
                .into_iter()
                .map(|session| {
                    serde_json::to_value(session).map_err(|error| error.to_string())
                })
                .collect(),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected external-session response: {other:?}")),
        }
    }

    fn admission(&self) -> Result<serde_json::Value, String> {
        match self.call(Request::Snapshot)? {
            Response::Snapshot { admission, .. } => {
                serde_json::to_value(admission).map_err(|error| error.to_string())
            }
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected snapshot response: {other:?}")),
        }
    }

    fn last_output(&self, id: &str, max_lines: usize) -> Result<String, String> {
        let data = match self.call(Request::Scrollback {
            id: SessionId(id.to_owned()),
        })? {
            Response::Scrollback { data } => data,
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected scrollback response: {other:?}")),
        };
        // The ring holds raw pty bytes including escape sequences. A sibling
        // agent wants readable text, and forwarding control bytes into a tool
        // result is how a terminal-shaped payload reaches a model.
        let text = String::from_utf8_lossy(&data);
        let plain = terminalai_core::mcp::strip_terminal_control(&text);
        let lines: Vec<&str> = plain.lines().filter(|line| !line.trim().is_empty()).collect();
        let start = lines.len().saturating_sub(max_lines);
        Ok(lines[start..].join("\n"))
    }

    fn write_session(&self, id: &str, data: &str) -> Result<(), String> {
        match self.call(Request::Write {
            id: SessionId(id.to_owned()),
            data: data.to_owned(),
        })? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected write response: {other:?}")),
        }
    }

    fn kill_session(&self, id: &str) -> Result<(), String> {
        match self.call(Request::Kill {
            id: SessionId(id.to_owned()),
        })? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected kill response: {other:?}")),
        }
    }

    fn log_mutation(&self, tool: &str, session: &str, allowed: bool, detail: &str) {
        // stderr, not stdout: stdout is the JSON-RPC channel and anything else
        // written there corrupts the stream. The daemon's own diagnostics
        // timeline records the resulting Write/Kill separately.
        let outcome = if allowed { "allowed" } else { "refused" };
        eprintln!("mcp mutation {outcome}: tool={tool} session={session} {detail}");
    }
}

fn cmd_pin(args: &[String]) -> i32 {
    let (machine, rest) = without_json(args);
    let Some(id) = rest.first() else {
        return control_usage("pin needs a session id");
    };
    match control_call(Request::TogglePin {
        id: SessionId(id.clone()),
    }) {
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// The parsed grid a pinned pane renders. Exposed so the split view's data path
/// can be exercised without a GUI.
fn cmd_grid(args: &[String]) -> i32 {
    let (machine, rest) = without_json(args);
    let Some(id) = rest.first() else {
        return control_usage("grid needs a session id");
    };
    match control_call(Request::GridSnapshot {
        id: SessionId(id.clone()),
    }) {
        Ok(Response::GridSnapshot { grid }) if machine => print_json(grid),
        Ok(Response::GridSnapshot { grid }) => {
            for line in &grid.lines {
                println!("{line}");
            }
            0
        }
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Read a session's output history, including bytes the in-memory ring has
/// already dropped. The one way to see the disk tier from outside the GUI.
/// Survey the checkouts this tool created that no live session owns.
fn cmd_worktrees(args: &[String]) -> i32 {
    let (machine, _) = without_json(args);
    match control_call(Request::StaleWorktrees) {
        Ok(Response::StaleWorktrees { worktrees }) if machine => {
            print_json(serde_json::json!({ "worktrees": worktrees }));
            0
        }
        Ok(Response::StaleWorktrees { worktrees }) => {
            if worktrees.is_empty() {
                println!("no leftover checkouts");
                return 0;
            }
            for stale in worktrees {
                println!("{:<28} {:?}  {}", stale.branch, stale.state, stale.path.display());
            }
            0
        }
        Ok(other) => print_control_response(other, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Retire one stopped row into the history, the same request the row's button
/// sends. Present so the whole archive path can be driven without a WebView.
fn cmd_archive(args: &[String]) -> i32 {
    let (machine, rest) = without_json(args);
    let Some(id) = rest.first() else {
        return control_usage("archive <session-id> [--json]");
    };
    match control_call(Request::Archive {
        id: SessionId(id.clone()),
    }) {
        Ok(other) => print_control_response(other, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Read the archive of finished sessions the same way the window does.
///
/// The window reaches this through a Tauri command; the probe reaches the same
/// request directly, so the record shape can be checked without a WebView.
fn cmd_archives(args: &[String]) -> i32 {
    let (machine, _) = without_json(args);
    match control_call(Request::SessionHistory) {
        Ok(Response::SessionHistory { archives }) if machine => {
            print_json(serde_json::json!({ "archives": archives }));
            0
        }
        Ok(Response::SessionHistory { archives }) => {
            if archives.is_empty() {
                println!("no archived sessions");
                return 0;
            }
            for archive in archives {
                println!(
                    "{}  {:<8} {:<24} {}",
                    archive.id.0,
                    format!("{:?}", archive.agent).to_lowercase(),
                    archive.name,
                    archive.command
                );
            }
            0
        }
        Ok(other) => print_control_response(other, machine),
        Err(error) => print_control_error(error, machine),
    }
}

/// Find a string in every session's retained output.
///
/// The headline case this exists for: "where did that error print" across
/// twenty sessions, which was a manual scroll through twenty panes.
fn cmd_search(args: &[String]) -> i32 {
    let (machine, rest) = without_json(args);
    let case_sensitive = rest.iter().any(|arg| arg == "--case");
    let needle: Vec<&String> = rest.iter().filter(|arg| *arg != "--case").collect();
    let Some(needle) = needle.first() else {
        return control_usage("search <needle> [--case] [--json]");
    };
    match control_call(Request::SearchScrollback {
        query: terminalai_core::search::SearchQuery {
            needle: (*needle).clone(),
            case_sensitive,
        },
        max_bytes: terminalai_daemon::MAX_HISTORY_BYTES,
    }) {
        Ok(Response::SearchResults {
            matches,
            searched_bytes,
        }) if machine => print_json(serde_json::json!({
            "searched_bytes_per_session": searched_bytes,
            "sessions": matches,
        })),
        Ok(Response::SearchResults { matches, .. }) => {
            if matches.is_empty() {
                println!("no session matched");
                return 0;
            }
            for session in matches {
                let more = if session.truncated { " (excerpts capped)" } else { "" };
                println!(
                    "{} {} — {} match(es){more}",
                    session.id.0, session.name, session.total_matches
                );
                for hit in session.hits {
                    println!("  {:>6}: {}", hit.line, hit.text);
                }
            }
            0
        }
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

fn cmd_history(args: &[String]) -> i32 {
    let (machine, rest) = without_json(args);
    let Some(id) = rest.first() else {
        return control_usage("history <session-id> [bytes] [--json]");
    };
    let max_bytes = match rest.get(1) {
        Some(value) => match value.parse::<u64>() {
            Ok(parsed) => parsed,
            Err(_) => return control_usage("history <session-id> [bytes] [--json]"),
        },
        None => terminalai_daemon::MAX_HISTORY_BYTES,
    };
    match control_call(Request::ScrollbackHistory {
        id: SessionId(id.clone()),
        max_bytes,
    }) {
        Ok(Response::ScrollbackHistory { data }) if machine => {
            print_json(serde_json::json!({
                "bytes": data.len(),
                "text": String::from_utf8_lossy(&data),
            }))
        }
        Ok(Response::ScrollbackHistory { data }) => {
            print!("{}", String::from_utf8_lossy(&data));
            0
        }
        Ok(response) => print_control_response(response, machine),
        Err(error) => print_control_error(error, machine),
    }
}

fn cmd_shutdown(args: &[String]) -> i32 {
    if !args.is_empty() {
        return control_usage("shutdown takes no arguments");
    }
    match control_call(Request::Shutdown) {
        Ok(Response::Ok) => {
            println!("TerminalAI daemon shutdown requested");
            0
        }
        Ok(response) => print_control_response(response, false),
        Err(error) => print_control_error(error, false),
    }
}

fn without_json(args: &[String]) -> (bool, Vec<String>) {
    let machine = args.iter().any(|arg| arg == "--json");
    let args = args
        .iter()
        .filter(|arg| arg.as_str() != "--json")
        .cloned()
        .collect();
    (machine, args)
}

fn one_control_argument(args: &[String], usage: &str) -> Result<String, String> {
    if args.len() != 1 {
        return Err(format!("usage: terminalai-probe {usage}"));
    }
    Ok(args[0].clone())
}

fn control_usage(message: &str) -> i32 {
    eprintln!("{message}\n\n{}", usage());  // placeholder");
    1
}

fn control_call(request: Request) -> Result<Response, String> {
    let timeout = Duration::from_secs(5);
    let client = DaemonClient::connect_with_timeout(timeout)
        .map_err(|error| format!("could not connect to TerminalAI daemon: {error}"))?;
    client
        .call_with_timeout(request, timeout)
        .map_err(|error| format!("TerminalAI daemon request failed: {error}"))
}

fn print_control_response(response: Response, machine: bool) -> i32 {
    let failed = matches!(response, Response::Error { .. });
    if machine {
        match serde_json::to_string(&response) {
            Ok(json) => println!("{json}"),
            Err(error) => return print_control_error(error.to_string(), true),
        }
    } else if let Response::Error { message } = &response {
        eprintln!("{message}");
    } else {
        println!("{response:?}");
    }
    i32::from(failed)
}

fn print_control_error(error: impl std::fmt::Display, machine: bool) -> i32 {
    if machine {
        let output = serde_json::json!({ "kind": "error", "message": error.to_string() });
        println!("{output}");
    } else {
        eprintln!("{error}");
    }
    1
}

fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~\r")
}

fn cmd_hook(args: &[String]) -> i32 {
    let Some(agent_arg) = args.first() else {
        eprintln!("usage: terminalai-probe hook <claude|codex>");
        return 1;
    };
    let agent = match agent_arg.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => {
            eprintln!("unknown hook agent: {other}");
            return 1;
        }
    };
    let mut input = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut input) {
        eprintln!("could not read hook input: {error}");
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
        // A path on stdout is how Claude Code takes a directive back from a
        // hook. Printed only when the daemon supplied one, so every other event
        // stays fire-and-forget: an adapter that wrote to stdout on an ordinary
        // event would be answering a question the agent did not ask.
        Ok(Response::Hook {
            worktree_path: Some(path),
            ..
        }) => println!("{}", serde_json::json!({ "worktreePath": path })),
        Ok(Response::Hook { .. }) | Ok(Response::Ok) => {}
        Ok(other) => eprintln!("ignoring unexpected hook response: {other:?}"),
        Err(error) => eprintln!("ignoring hook delivery failure: {error}"),
    }
    0
}

fn cmd_hooks(args: &[String]) -> i32 {
    let Some(operation) = args.first().map(String::as_str) else {
        eprintln!("usage: terminalai-probe hooks <status|preview|install|remove> <claude|codex>");
        return 1;
    };
    let Some(agent_arg) = args.get(1) else {
        eprintln!("missing hook agent");
        return 1;
    };
    let agent = match agent_arg.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => {
            eprintln!("unknown hook agent: {other}");
            return 1;
        }
    };
    let mut config = None;
    let mut executable = None;
    let mut it = args[2..].iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--config" => config = it.next().map(PathBuf::from),
            "--executable" => executable = it.next().map(PathBuf::from),
            other => {
                eprintln!("unknown hooks option: {other}");
                return 1;
            }
        }
    }

    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let config = config
        .unwrap_or_else(|| terminalai_core::hook_config_path(agent, &home, codex_home.as_deref()));
    let executable = executable
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("terminalai-probe"));
    let endpoint = if agent == Agent::Claude {
        DaemonClient::connect_with_timeout(Duration::from_millis(750))
            .ok()
            .and_then(|client| client.hook_endpoint().ok())
    } else {
        None
    };
    let transport = hook_transport(agent, &executable, endpoint.as_ref());

    match operation {
        "preview" => {
            println!(
                "{}",
                terminalai_core::hook_config_preview(agent, &executable)
            );
            0
        }
        "status" => {
            match terminalai_core::hook_status_at_with_transport(agent, &config, &transport) {
                Ok(status) => print_json(status),
                Err(error) => print_error(error),
            }
        }
        "install" => {
            match terminalai_core::install_hooks_at_with_transport(agent, &config, &transport) {
                Ok(change) => print_json(change),
                Err(error) if matches!(transport, HookTransport::Http { .. }) => {
                    match terminalai_core::install_hooks_at(agent, &config, &executable) {
                        Ok(change) => print_json(change),
                        Err(fallback) => print_error(format!(
                            "HTTP hook install failed: {error}; command fallback failed: {fallback}"
                        )),
                    }
                }
                Err(error) => print_error(error),
            }
        }
        "remove" => match terminalai_core::remove_hooks_at(agent, &config, &executable) {
            Ok(change) => print_json(change),
            Err(error) => print_error(error),
        },
        other => {
            eprintln!("unknown hooks operation: {other}");
            1
        }
    }
}

fn hook_transport(
    agent: Agent,
    executable: &std::path::Path,
    endpoint: Option<&HookEndpoint>,
) -> HookTransport {
    if agent == Agent::Claude {
        if let Some(endpoint) = endpoint {
            return HookTransport::Http {
                url: endpoint.url_for(agent),
                host: endpoint.host.clone(),
                bearer_token: endpoint.bearer_token.clone(),
            };
        }
    }
    HookTransport::Command {
        executable: executable.to_path_buf(),
    }
}

fn print_json<T: serde::Serialize>(value: T) -> i32 {
    match serde_json::to_string_pretty(&value) {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => {
            eprintln!("could not encode hook result: {error}");
            1
        }
    }
}

fn print_error(error: impl std::fmt::Display) -> i32 {
    eprintln!("{error}");
    1
}

/// Ask each installed agent whether it is still authenticated.
///
/// The GUI cannot be unit-tested against a live login, so this is where the
/// probe's real behaviour gets verified: it runs the same code path the daemon
/// runs on its timer.
fn cmd_auth() -> i32 {
    for a in [Agent::Claude, Agent::Codex] {
        match agent::resolve(a, None) {
            Ok(b) => {
                let auth = terminalai_core::auth::probe(a, &b.path);
                println!(
                    "{:<12} {:?}{}{}",
                    a.command_name(),
                    auth.state,
                    auth.account
                        .map(|account| format!("  {account}"))
                        .unwrap_or_default(),
                    auth.detail
                        .map(|detail| format!("  ({detail})"))
                        .unwrap_or_default(),
                );
            }
            // Not installed is preflight's business, not an auth failure.
            Err(e) => println!("{:<12} not resolved: {e}", a.command_name()),
        }
    }
    0
}

fn cmd_resolve() -> i32 {
    let mut failed = false;
    for a in [Agent::Claude, Agent::Codex] {
        match agent::resolve(a, None) {
            Ok(b) => println!(
                "{:<12} {:?}  {}",
                a.command_name(),
                b.origin,
                b.path.display()
            ),
            Err(e) => {
                eprintln!("{:<12} FAILED: {e}", a.command_name());
                failed = true;
            }
        }
    }
    if failed {
        2
    } else {
        0
    }
}

/// Run an arbitrary program on a ConPTY. Isolates "the pty layer is broken"
/// from "this agent behaves oddly under a pty" when a launch misbehaves.
fn cmd_exec(args: &[String]) -> i32 {
    let Some(program) = args.first() else {
        eprintln!("usage: terminalai-probe exec <program> [args...]");
        return 1;
    };
    let cmd = ResolvedCommand {
        program: PathBuf::from(program),
        args: args[1..].to_vec(),
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    println!("{}", cmd.preview());
    println!("--- conpty ---");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let session = match PtySession::spawn(&cmd, pty::default_size(), move |c| {
        let _ = tx.send(c.to_vec());
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 3;
        }
    };
    let (out, exit) = drain(&session, rx, Duration::from_secs(30));
    print!("{}", String::from_utf8_lossy(&out));
    println!(
        "\n--- exit: {} ---",
        exit.map(|c| c.to_string())
            .unwrap_or_else(|| "killed".into())
    );
    if exit == Some(0) {
        0
    } else {
        3
    }
}

/// Measure what supervising N idle sessions costs this process.
///
/// Children are separate processes, so the CPU time reported here is entirely
/// TerminalAI's own supervision overhead — reader threads plus one exit watcher
/// per session. `--poll` reproduces the old 50 ms `try_wait` loop so the two
/// strategies can be compared on the same machine in the same run.
fn cmd_cpu_idle(args: &[String]) -> i32 {
    let mut sessions = 10usize;
    let mut seconds = 10u64;
    let mut poll = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--sessions" => {
                index += 1;
                match args.get(index).and_then(|value| value.parse().ok()) {
                    Some(value) => sessions = value,
                    None => {
                        eprintln!("--sessions needs a number");
                        return 1;
                    }
                }
            }
            "--seconds" => {
                index += 1;
                match args.get(index).and_then(|value| value.parse().ok()) {
                    Some(value) => seconds = value,
                    None => {
                        eprintln!("--seconds needs a number");
                        return 1;
                    }
                }
            }
            "--poll" => poll = true,
            other => {
                eprintln!("unknown option: {other}");
                return 1;
            }
        }
        index += 1;
    }

    // A child that blocks rather than spins, so the sample measures supervision
    // and not the child's own work.
    let cmd = ResolvedCommand {
        program: PathBuf::from("cmd.exe"),
        args: vec!["/c".into(), "pause".into()],
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let mut live = Vec::with_capacity(sessions);
    let output_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    for _ in 0..sessions {
        let counter = output_bytes.clone();
        match PtySession::spawn(&cmd, pty::default_size(), move |chunk| {
            counter.fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
        }) {
            Ok(session) => live.push(std::sync::Arc::new(session)),
            Err(error) => {
                eprintln!("could not spawn idle session: {error}");
                return 3;
            }
        }
    }

    let watching = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut watchers = Vec::with_capacity(live.len());
    for session in &live {
        let session = session.clone();
        let watching = watching.clone();
        let watcher = std::thread::Builder::new()
            .name(format!("terminalai-probe-watch-{}", watchers.len()))
            .spawn(move || {
            if poll {
                // The pre-2026-08-03 supervision loop, kept for comparison only.
                while watching.load(std::sync::atomic::Ordering::Relaxed) {
                    match session.try_wait() {
                        Ok(Some(_)) => return,
                        Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                        Err(_) => std::thread::sleep(Duration::from_millis(250)),
                    }
                }
            } else {
                let _ = session.wait_for_exit();
            }
        });
        match watcher {
            Ok(watcher) => watchers.push(watcher),
            Err(error) => {
                eprintln!("could not start a session watcher: {error}");
                return 3;
            }
        }
    }

    // Let the children finish drawing before sampling.
    std::thread::sleep(Duration::from_secs(1));
    let before = process_cpu_time();
    std::thread::sleep(Duration::from_secs(seconds));
    let after = process_cpu_time();

    watching.store(false, std::sync::atomic::Ordering::Relaxed);
    for session in &live {
        let _ = session.kill();
    }
    for watcher in watchers {
        let _ = watcher.join();
    }

    match (before, after) {
        (Some(before), Some(after)) => {
            let used = after.saturating_sub(before);
            let strategy = if poll { "poll(50ms)" } else { "wait-on-handle" };
            println!(
                "strategy={strategy} sessions={sessions} window={seconds}s cpu={:.1}ms cpu_per_session_per_s={:.3}ms output_bytes={}",
                used.as_secs_f64() * 1000.0,
                used.as_secs_f64() * 1000.0 / (sessions.max(1) as f64 * seconds.max(1) as f64),
                output_bytes.load(std::sync::atomic::Ordering::Relaxed),
            );
            0
        }
        _ => {
            eprintln!("could not read process CPU time on this platform");
            1
        }
    }
}

#[derive(Debug, Serialize)]
struct HygieneReport {
    sessions: usize,
    method: &'static str,
    supervised_conpty: HygieneRun,
    terminal_launched: HygieneRun,
}

#[derive(Debug, Serialize)]
struct HygieneRun {
    console_windows_created: usize,
    input_latency_ms: LatencySummary,
}

#[derive(Debug, Serialize)]
struct LatencySummary {
    samples: usize,
    min: f64,
    median: f64,
    max: f64,
}

impl LatencySummary {
    fn from_durations(mut values: Vec<Duration>) -> Self {
        values.sort_unstable();
        let milliseconds = values
            .iter()
            .map(Duration::as_secs_f64)
            .map(|seconds| seconds * 1000.0)
            .collect::<Vec<_>>();
        let samples = milliseconds.len();
        let median = match samples {
            0 => 0.0,
            count if count % 2 == 0 => {
                (milliseconds[count / 2 - 1] + milliseconds[count / 2]) / 2.0
            }
            count => milliseconds[count / 2],
        };
        Self {
            samples,
            min: milliseconds.first().copied().unwrap_or(0.0),
            median,
            max: milliseconds.last().copied().unwrap_or(0.0),
        }
    }
}

fn cmd_hygiene(args: &[String]) -> i32 {
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("hygiene measurement is currently implemented for Windows only");
        return 1;
    }
    #[cfg(windows)]
    {
        cmd_hygiene_windows(args)
    }
}

#[cfg(windows)]
fn cmd_hygiene_windows(args: &[String]) -> i32 {
    let mut sessions = 8usize;
    let mut machine = false;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--sessions" => {
                index += 1;
                sessions = match args.get(index).and_then(|value| value.parse().ok()) {
                    Some(value @ 1..=32) => value,
                    _ => {
                        eprintln!("--sessions must be between 1 and 32");
                        return 1;
                    }
                };
            }
            "--json" => machine = true,
            "--output" => {
                index += 1;
                output = match args.get(index) {
                    Some(value) => Some(PathBuf::from(value)),
                    None => {
                        eprintln!("--output needs a path");
                        return 1;
                    }
                };
            }
            other => {
                eprintln!("unknown option: {other}");
                return 1;
            }
        }
        index += 1;
    }

    let supervised = match measure_supervised_conpty(sessions) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("supervised ConPTY measurement failed: {error}");
            return 3;
        }
    };
    let terminal = match measure_terminal_launches(sessions) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("terminal-launched measurement failed: {error}");
            return 3;
        }
    };
    let report = HygieneReport {
        sessions,
        method: "ConsoleWindowClass snapshots on the current desktop, falling back to new conhost.exe process snapshots when the isolated desktop does not enumerate console windows, plus marker round trips over redirected stdin/stdout siblings using the same CREATE_NEW_CONSOLE launch flag",
        supervised_conpty: supervised,
        terminal_launched: terminal,
    };
    let json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("could not encode hygiene report: {error}");
            return 1;
        }
    };
    if let Some(path) = output {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("could not create hygiene output directory: {error}");
                return 1;
            }
        }
        if let Err(error) = std::fs::write(&path, &json) {
            eprintln!("could not write hygiene report {}: {error}", path.display());
            return 1;
        }
    }
    if machine {
        println!("{json}");
    } else {
        println!(
            "supervised_conpty: {} console windows, median input {:.3} ms",
            report.supervised_conpty.console_windows_created,
            report.supervised_conpty.input_latency_ms.median
        );
        println!(
            "terminal_launched: {} console windows, median input {:.3} ms",
            report.terminal_launched.console_windows_created,
            report.terminal_launched.input_latency_ms.median
        );
    }
    0
}

#[cfg(windows)]
fn hygiene_command() -> ResolvedCommand {
    ResolvedCommand {
        program: PathBuf::from("cmd.exe"),
        args: vec!["/d".into(), "/q".into(), "/k".into()],
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

#[cfg(windows)]
fn marker_round_trip(
    rx: &mpsc::Receiver<Vec<u8>>,
    marker: &[u8],
    started: std::time::Instant,
) -> Result<Duration, String> {
    let deadline = started + Duration::from_secs(5);
    let mut output = Vec::new();
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(chunk) => {
                output.extend_from_slice(&chunk);
                if output.windows(marker.len()).any(|window| window == marker) {
                    return Ok(started.elapsed());
                }
                if output.len() > 64 * 1024 {
                    output.drain(..output.len() - 64 * 1024);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("child output reader disconnected".into())
            }
        }
    }
    Err(format!(
        "child did not echo marker {} within 5 seconds",
        String::from_utf8_lossy(marker)
    ))
}

#[cfg(windows)]
fn measure_supervised_conpty(sessions: usize) -> Result<HygieneRun, String> {
    let mut tracker = ConsoleWindowTracker::new();
    let mut live = Vec::with_capacity(sessions);
    for index in 0..sessions {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let session = PtySession::spawn(&hygiene_command(), pty::default_size(), move |chunk| {
            let _ = tx.send(chunk.to_vec());
        })
        .map_err(|error| error.to_string())?;
        live.push((session, rx));
        tracker.sample();
        if index == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let mut latencies = Vec::with_capacity(sessions);
    for (index, (session, rx)) in live.iter().enumerate() {
        let marker = format!("TERMINALAI_SUPERVISED_{index}");
        let started = std::time::Instant::now();
        session
            .write(format!("echo {marker}\r").as_bytes())
            .map_err(|error| error.to_string())?;
        latencies.push(marker_round_trip(rx, marker.as_bytes(), started)?);
        tracker.sample();
    }
    for (session, _) in &live {
        let _ = session.kill();
    }
    for _ in 0..10 {
        tracker.sample();
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(HygieneRun {
        console_windows_created: tracker.created(false),
        input_latency_ms: LatencySummary::from_durations(latencies),
    })
}

#[cfg(windows)]
struct TerminalLaunch {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::Receiver<Vec<u8>>,
}

#[cfg(windows)]
impl Drop for TerminalLaunch {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
fn spawn_terminal_launch() -> Result<TerminalLaunch, String> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd.exe");
    command
        .args(["/d", "/q", "/k"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "terminal baseline did not expose stdin".to_string())?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "terminal baseline did not expose stdout".to_string())?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("terminalai-probe-baseline-reader".into())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(size) if tx.send(buffer[..size].to_vec()).is_err() => break,
                    Ok(_) => {}
                }
            }
        })
        .map_err(|error| format!("could not start the terminal baseline reader: {error}"))?;
    Ok(TerminalLaunch { child, stdin, rx })
}

#[cfg(windows)]
fn measure_terminal_launches(sessions: usize) -> Result<HygieneRun, String> {
    let console_windows_created = measure_terminal_console_windows(sessions)?;
    let input_latency_ms = measure_terminal_round_trips(sessions)?;
    Ok(HygieneRun {
        console_windows_created,
        input_latency_ms: LatencySummary::from_durations(input_latency_ms),
    })
}

#[cfg(windows)]
fn measure_terminal_console_windows(sessions: usize) -> Result<usize, String> {
    use std::os::windows::process::CommandExt;
    let mut tracker = ConsoleWindowTracker::new();
    let mut children = Vec::with_capacity(sessions);
    for _ in 0..sessions {
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/q", "/c", "ping.exe 127.0.0.1 -n 5 >nul"])
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE);
        children.push(command.spawn().map_err(|error| error.to_string())?);
        tracker.sample();
        std::thread::sleep(Duration::from_millis(50));
    }
    for _ in 0..20 {
        tracker.sample();
        std::thread::sleep(Duration::from_millis(25));
    }
    for child in &mut children {
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
    tracker.sample();
    Ok(tracker.created(true))
}

#[cfg(windows)]
fn measure_terminal_round_trips(sessions: usize) -> Result<Vec<Duration>, String> {
    let mut tracker = ConsoleWindowTracker::new();
    let mut live = Vec::with_capacity(sessions);
    for _ in 0..sessions {
        live.push(spawn_terminal_launch()?);
        tracker.sample();
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut latencies = Vec::with_capacity(sessions);
    for (index, session) in live.iter_mut().enumerate() {
        let marker = format!("TERMINALAI_TERMINAL_{index}");
        let started = std::time::Instant::now();
        session
            .stdin
            .write_all(format!("echo {marker}\r\n").as_bytes())
            .map_err(|error| error.to_string())?;
        session.stdin.flush().map_err(|error| error.to_string())?;
        latencies.push(marker_round_trip(&session.rx, marker.as_bytes(), started)?);
        tracker.sample();
    }
    for _ in 0..10 {
        tracker.sample();
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(latencies)
}

#[cfg(windows)]
struct ConsoleWindowTracker {
    baseline: std::collections::HashSet<isize>,
    created: std::collections::HashSet<isize>,
    baseline_hosts: std::collections::HashSet<u32>,
    created_hosts: std::collections::HashSet<u32>,
}

#[cfg(windows)]
impl ConsoleWindowTracker {
    fn new() -> Self {
        Self {
            baseline: console_windows(),
            created: std::collections::HashSet::new(),
            baseline_hosts: console_hosts(),
            created_hosts: std::collections::HashSet::new(),
        }
    }

    fn sample(&mut self) {
        for handle in console_windows() {
            if !self.baseline.contains(&handle) {
                self.created.insert(handle);
            }
        }
        for process_id in console_hosts() {
            if !self.baseline_hosts.contains(&process_id) {
                self.created_hosts.insert(process_id);
            }
        }
    }

    fn created(&self, allow_host_fallback: bool) -> usize {
        if allow_host_fallback && self.created.is_empty() {
            self.created_hosts.len()
        } else {
            self.created.len()
        }
    }
}

#[cfg(windows)]
fn console_hosts() -> std::collections::HashSet<u32> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return std::collections::HashSet::new();
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut hosts = std::collections::HashSet::new();
    let first = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    if first {
        loop {
            let length = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            if name.eq_ignore_ascii_case("conhost.exe") {
                hosts.insert(entry.th32ProcessID);
            }
            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }
    }
    unsafe {
        CloseHandle(snapshot);
    }
    hosts
}

#[cfg(windows)]
fn console_windows() -> std::collections::HashSet<isize> {
    // windows-sys 0.61 dropped the `BOOL` alias; the callback contract is a
    // raw `i32` where nonzero means "keep enumerating".
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::StationsAndDesktops::{EnumDesktopWindows, GetThreadDesktop};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, IsWindowVisible};

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut class = [0u16; 64];
        let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        if length > 0 && String::from_utf16_lossy(&class[..length as usize]) == "ConsoleWindowClass"
        {
            let handles = &mut *(lparam as *mut std::collections::HashSet<isize>);
            handles.insert(hwnd as isize);
        }
        1
    }

    let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    if desktop.is_null() {
        return std::collections::HashSet::new();
    }
    let mut handles = std::collections::HashSet::new();
    unsafe {
        EnumDesktopWindows(desktop, Some(collect), &mut handles as *mut _ as LPARAM);
    }
    handles
}

/// Kernel plus user CPU time consumed by this process so far.
fn process_cpu_time() -> Option<Duration> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

        let mut creation = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut exit = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut kernel = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut user = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if ok == 0 {
            return None;
        }
        let ticks = |time: FILETIME| {
            (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
        };
        // FILETIME counts 100-nanosecond intervals.
        Some(Duration::from_nanos(
            (ticks(kernel) + ticks(user)).saturating_mul(100),
        ))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[derive(Debug, Serialize)]
struct FleetStressOutput {
    profile: terminalai_core::registry::stress::FleetStressReport,
    resources: FleetStressResources,
}

#[derive(Debug, Serialize)]
struct FleetStressResources {
    cpu_ms: Option<f64>,
    working_set_before_bytes: Option<u64>,
    working_set_after_bytes: Option<u64>,
    working_set_delta_bytes: Option<u64>,
    cpu_budget_ms: f64,
    working_set_delta_budget_bytes: u64,
    cpu_under_budget: Option<bool>,
    working_set_under_budget: Option<bool>,
    /// False on targets where this process cannot read its own resource
    /// counters. The logical registry gates still run there; Windows release
    /// verification requires this to be true.
    enforced: bool,
    all_pass: bool,
}

const FLEET_STRESS_CPU_BUDGET: Duration = Duration::from_secs(15);
const FLEET_STRESS_WORKING_SET_BUDGET: u64 = 256 * 1024 * 1024;

fn cmd_fleet_stress(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let mut sessions = terminalai_core::registry::stress::DEFAULT_SESSIONS;
    let mut events_per_session = terminalai_core::registry::stress::DEFAULT_EVENTS_PER_SESSION;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--sessions" => {
                index += 1;
                sessions = match args.get(index).and_then(|value| value.parse().ok()) {
                    Some(value) => value,
                    None => {
                        eprintln!("--sessions needs a number");
                        return 1;
                    }
                };
            }
            "--events-per-session" => {
                index += 1;
                events_per_session = match args.get(index).and_then(|value| value.parse().ok()) {
                    Some(value) => value,
                    None => {
                        eprintln!("--events-per-session needs a number");
                        return 1;
                    }
                };
            }
            "--output" => {
                index += 1;
                output = match args.get(index) {
                    Some(value) => Some(PathBuf::from(value)),
                    None => {
                        eprintln!("--output needs a path");
                        return 1;
                    }
                };
            }
            other => {
                eprintln!("unknown option: {other}");
                return 1;
            }
        }
        index += 1;
    }

    let cpu_before = process_cpu_time();
    let working_set_before = process_working_set_bytes();
    let profile = match terminalai_core::registry::stress::run(sessions, events_per_session) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("fleet stress failed: {error}");
            return 3;
        }
    };
    let cpu_ms = cpu_before
        .zip(process_cpu_time())
        .map(|(before, after)| after.saturating_sub(before).as_secs_f64() * 1000.0);
    let working_set_after = process_working_set_bytes();
    let working_set_delta = working_set_before.zip(working_set_after).map(|(before, after)| {
        after.saturating_sub(before)
    });
    let cpu_under_budget = cpu_ms.map(|value| value <= FLEET_STRESS_CPU_BUDGET.as_secs_f64() * 1000.0);
    let working_set_under_budget = working_set_delta.map(|value| value <= FLEET_STRESS_WORKING_SET_BUDGET);
    let enforced = cpu_under_budget.is_some() && working_set_under_budget.is_some();
    let resources = FleetStressResources {
        cpu_ms,
        working_set_before_bytes: working_set_before,
        working_set_after_bytes: working_set_after,
        working_set_delta_bytes: working_set_delta,
        cpu_budget_ms: FLEET_STRESS_CPU_BUDGET.as_secs_f64() * 1000.0,
        working_set_delta_budget_bytes: FLEET_STRESS_WORKING_SET_BUDGET,
        cpu_under_budget,
        working_set_under_budget,
        enforced,
        all_pass: cpu_under_budget.unwrap_or(true) && working_set_under_budget.unwrap_or(true),
    };
    let result = FleetStressOutput { profile, resources };
    let json = match serde_json::to_string_pretty(&result) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("could not encode fleet stress report: {error}");
            return 1;
        }
    };
    if let Some(path) = output {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("could not create fleet stress output directory: {error}");
                return 1;
            }
        }
        if let Err(error) = std::fs::write(&path, &json) {
            eprintln!("could not write fleet stress report {}: {error}", path.display());
            return 1;
        }
    }
    if machine {
        println!("{json}");
    } else {
        println!(
            "fleet-stress: {} sessions, {} events, startup {:.1} ms, hook p95 {:.3} ms, snapshot p95 {:.3} ms",
            result.profile.sessions,
            result.profile.events,
            result.profile.startup_ms,
            result.profile.hooks.p95_ms,
            result.profile.snapshots.p95_ms,
        );
        println!(
            "gates: logical={} resources={} enforced={}",
            result.profile.gates.all_pass, result.resources.all_pass, result.resources.enforced
        );
    }
    if result.profile.gates.all_pass && result.resources.all_pass {
        0
    } else {
        3
    }
}

fn process_working_set_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        use std::mem::size_of;
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        };
        if ok != 0 {
            return Some(counters.WorkingSetSize as u64);
        }
    }
    None
}

fn cmd_build(args: &[String], run: bool) -> i32 {
    let (spec, timeout) = match parse_launch_spec(args, true) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}\n\n{}", usage());  // placeholder");
            return 1;
        }
    };
    let which = spec.agent;

    let binary = match agent::resolve(which, None) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let cmd = match spec.resolve(&binary) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    println!("{}", cmd.preview());
    if !run {
        return 0;
    }
    println!("--- conpty ---");

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let session = match PtySession::spawn(&cmd, pty::default_size(), move |chunk| {
        let _ = tx.send(chunk.to_vec());
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 3;
        }
    };

    let (out, exit) = drain(&session, rx, timeout);
    print!("{}", String::from_utf8_lossy(&out));
    println!(
        "\n--- exit: {} ---",
        exit.map(|c| c.to_string())
            .unwrap_or_else(|| "killed".into())
    );
    if exit == Some(0) {
        0
    } else {
        3
    }
}

fn parse_launch_spec(
    args: &[String],
    allow_timeout: bool,
) -> Result<(LaunchSpec, Duration), String> {
    let Some(agent_arg) = args.first() else {
        return Err("missing agent".into());
    };
    let agent = match agent_arg.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => return Err(format!("unknown agent: {other}")),
    };
    let mut spec = LaunchSpec {
        agent,
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ..Default::default()
    };
    let mut timeout = Duration::from_secs(30);
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        index += 1;
        match flag.as_str() {
            "--cwd" => spec.cwd = PathBuf::from(take_value(args, &mut index, flag)?),
            "--model" => spec.model = Some(take_value(args, &mut index, flag)?),
            "--name" => spec.name = Some(take_value(args, &mut index, flag)?),
            "--prompt" => spec.initial_prompt = Some(take_value(args, &mut index, flag)?),
            "--worktree" => {
                spec.worktree = true;
            }
            "--agent-home" => {
                spec.agent_home = Some(PathBuf::from(take_value(args, &mut index, flag)?));
            }
            "--env-passthrough" => {
                // Repeatable, one name per use. A list separator would make the
                // names ambiguous on the one platform whose PATH uses both.
                spec.env_passthrough
                    .push(take_value(args, &mut index, flag)?);
            }
            "--setup-hook" => {
                spec.environment.setup = Some(take_value(args, &mut index, flag)?);
            }
            "--teardown-hook" => {
                spec.environment.teardown = Some(take_value(args, &mut index, flag)?);
            }
            "--port-base" => {
                spec.environment.port_base = take_value(args, &mut index, flag)?
                    .parse()
                    .map_err(|_| "--port-base must be an integer".to_string())?;
            }
            "--port-count" => {
                spec.environment.port_count = take_value(args, &mut index, flag)?
                    .parse()
                    .map_err(|_| "--port-count must be an integer".to_string())?;
            }
            "--timeout" if allow_timeout => {
                let value = take_value(args, &mut index, flag)?;
                timeout = Duration::from_secs(
                    value
                        .parse()
                        .map_err(|_| "--timeout must be a whole number of seconds".to_string())?,
                );
            }
            "--timeout" => return Err("--timeout is only supported by spawn".into()),
            "--effort" => {
                let value = take_value(args, &mut index, flag)?;
                spec.effort = Some(match value.as_str() {
                    "low" => Effort::Low,
                    "medium" => Effort::Medium,
                    "high" => Effort::High,
                    "xhigh" => Effort::XHigh,
                    "max" => Effort::Max,
                    _ => Effort::Custom(value),
                });
            }
            "--permission" => {
                spec.permission = Some(Permission::parse(&take_value(args, &mut index, flag)?));
            }
            "--sandbox" => {
                spec.sandbox = Some(match take_value(args, &mut index, flag)?.as_str() {
                    "read-only" => Sandbox::ReadOnly,
                    "workspace-write" => Sandbox::WorkspaceWrite,
                    "danger-full-access" => Sandbox::DangerFullAccess,
                    other => return Err(format!("bad --sandbox: {other}")),
                });
            }
            "--raw" => {
                spec.extra_args.extend(args[index..].iter().cloned());
                break;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok((spec, timeout))
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))?;
    *index += 1;
    Ok(value)
}

/// Read until the child exits or the timeout expires.
///
/// Exit is detected with `try_wait`, never by the read side hitting EOF: on
/// Windows the ConPTY master stays readable after the child is gone, because
/// conhost — not the child — owns the far end of the pipe. Waiting for EOF here
/// hangs forever.
fn drain(
    session: &PtySession,
    rx: mpsc::Receiver<Vec<u8>>,
    timeout: Duration,
) -> (Vec<u8>, Option<u32>) {
    let deadline = std::time::Instant::now() + timeout;
    let mut out = Vec::new();
    let mut exit = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                out.extend_from_slice(&chunk);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Ok(Some(code)) = session.try_wait() {
            // Give conhost a moment to flush whatever the child wrote last.
            let settle = std::time::Instant::now() + Duration::from_millis(300);
            while std::time::Instant::now() < settle {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(chunk) => out.extend_from_slice(&chunk),
                    Err(_) => continue,
                }
            }
            exit = Some(code);
            break;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("--- timed out after {}s, killing ---", timeout.as_secs());
            let _ = session.kill();
            break;
        }
    }
    (out, exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use terminalai_core::{parse_hook, HookNotification, HookSignal};

    /// Every dispatched subcommand appears in the help, because the help is
    /// built from the same table the dispatcher reads.
    ///
    /// The old shape was a hand-written `USAGE` constant beside a `match`, and
    /// the two had drifted: twenty-nine arms, twenty-six advertised, with
    /// `auth`, `exec` and `limits` reachable and undocumented. This asserts the
    /// property rather than the list, so it cannot go stale the way the list
    /// did.
    #[test]
    fn the_help_is_the_dispatch_table() {
        let help = usage();
        for command in COMMANDS {
            assert!(
                help.contains(&format!("terminalai-probe {}", command.name)),
                "{} dispatches but does not appear in the help",
                command.name
            );
            assert!(
                command.synopsis.starts_with("terminalai-probe "),
                "{}'s synopsis does not read as a command line: {:?}",
                command.name,
                command.synopsis
            );
        }
        // And no two arms answer to the same word, which a `match` would have
        // caught with an unreachable-pattern warning and a table does not.
        let mut names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two subcommands share a name");
    }

    #[test]
    fn the_help_still_carries_the_option_reference() {
        // The generated half is the command list; the options below it are
        // prose and stay written by hand. Losing them would be silent.
        let help = usage();
        assert!(help.contains("OPTIONS:"), "{help}");
        assert!(help.contains("HOOK OPTIONS:"), "{help}");
        assert!(help.contains("--raw <arg>..."), "{help}");
    }

    #[test]
    fn parses_claude_notification_payload() {
        let event = parse_hook(
            Agent::Claude,
            r#"{"session_id":"cc-1","cwd":"C:\\repo","hook_event_name":"Notification","notification_type":"permission_prompt"}"#,
        )
        .expect("hook");
        assert_eq!(event.session_id.as_deref(), Some("cc-1"));
        assert_eq!(
            event.signal,
            HookSignal::Notification {
                notification: HookNotification::PermissionPrompt
            }
        );
    }

    #[test]
    fn parses_codex_permission_request_and_aliases() {
        let event = parse_hook(
            Agent::Codex,
            r#"{"thread_id":"cx-1","event":"PermissionRequest","type":"approval_request"}"#,
        )
        .expect("hook");
        assert_eq!(event.session_id.as_deref(), Some("cx-1"));
        assert_eq!(
            event.signal,
            HookSignal::Notification {
                notification: HookNotification::PermissionPrompt
            }
        );
    }

    #[test]
    fn control_json_flag_is_removed_without_reordering_arguments() {
        let args = vec![
            "claude".into(),
            "--json".into(),
            "--model".into(),
            "opus".into(),
        ];
        let (machine, args) = without_json(&args);
        assert!(machine);
        assert_eq!(args, ["claude", "--model", "opus"]);
    }

    #[test]
    fn start_parser_builds_the_same_launch_spec_as_the_probe() {
        let args = vec![
            "codex".into(),
            "--cwd".into(),
            ".".into(),
            "--model".into(),
            "gpt-5.1-codex".into(),
            "--effort".into(),
            "high".into(),
            "--raw".into(),
            "--search".into(),
        ];
        let (spec, timeout) = parse_launch_spec(&args, false).expect("launch spec");
        assert_eq!(spec.agent, Agent::Codex);
        assert_eq!(spec.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(spec.effort, Some(Effort::High));
        assert_eq!(spec.extra_args, ["--search"]);
        assert_eq!(timeout, Duration::from_secs(30));
    }

    #[test]
    fn start_parser_keeps_environment_hooks_and_port_settings() {
        let args = vec![
            "claude".into(),
            "--setup-hook".into(),
            "pnpm env:up".into(),
            "--teardown-hook".into(),
            "pnpm env:down".into(),
            "--port-base".into(),
            "43000".into(),
            "--port-count".into(),
            "2".into(),
        ];
        let (spec, _) = parse_launch_spec(&args, false).expect("launch spec");
        assert_eq!(spec.environment.setup.as_deref(), Some("pnpm env:up"));
        assert_eq!(spec.environment.teardown.as_deref(), Some("pnpm env:down"));
        assert_eq!(spec.environment.port_base, 43_000);
        assert_eq!(spec.environment.port_count, 2);
    }

    #[test]
    fn hygiene_latency_summary_sorts_and_averages_even_samples() {
        let summary = LatencySummary::from_durations(vec![
            Duration::from_millis(4),
            Duration::from_millis(1),
            Duration::from_millis(3),
            Duration::from_millis(2),
        ]);
        assert_eq!(summary.samples, 4);
        assert_eq!(summary.min, 1.0);
        assert_eq!(summary.median, 2.5);
        assert_eq!(summary.max, 4.0);
    }

    #[test]
    fn send_uses_the_same_bracketed_paste_contract_as_the_gui() {
        assert_eq!(bracketed_paste("hello"), "\x1b[200~hello\x1b[201~\r");
    }

    #[test]
    fn control_responses_are_single_line_json() {
        let response = Response::Launched {
            id: terminalai_core::SessionId("s0001".into()),
            queued: false,
        };
        assert_eq!(
            serde_json::to_string(&response).expect("response JSON"),
            r#"{"kind":"launched","id":"s0001","queued":false}"#
        );
    }
}

/// Does the installed agent accept the versioned compatibility matrix?
///
/// The core tests consume the same fixture cases to prove emitted argv shape.
/// This machine-facing half adds the facts that cannot belong in a deterministic
/// unit test: which exact CLI version is installed, whether its help lists every
/// emitted flag, and whether listed options are usable in the mode we launch.
/// Older fixtures are skipped when a newer matching fixture exists; an agent with
/// no exact fixture is still a failure rather than an unverified pass.
///
/// Exit codes: 0 clean, 1 usage, 2 the agent could not be resolved or asked, 3
/// the installed version or compatibility matrix disagrees with the fixture.
fn cmd_verify_goldens(args: &[String]) -> i32 {
    let (machine, args) = without_json(args);
    let mut goldens = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../terminalai-core/tests/fixtures/launch");
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--goldens" => {
                let Some(value) = args.get(index + 1) else {
                    return control_usage("--goldens needs a directory");
                };
                goldens = PathBuf::from(value);
                index += 2;
            }
            other => return control_usage(&format!("unknown argument: {other}")),
        }
    }

    // Read the directory rather than naming the files. A hand-written list
    // would quietly stop covering a golden the day one is added, and report
    // clean for every run after that.
    let entries = match std::fs::read_dir(&goldens) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("cannot read goldens at {}: {error}", goldens.display());
            return 2;
        }
    };
    let mut fixtures: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    fixtures.sort();
    if fixtures.is_empty() {
        eprintln!("no golden fixtures under {}", goldens.display());
        return 2;
    }

    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../terminalai-core")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../terminalai-core"));
    let mut reports: Vec<GoldenReport> = Vec::new();
    let mut matched_agents: Vec<Agent> = Vec::new();
    let mut worst = 0;
    for fixture in &fixtures {
        let name = fixture
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(fixture) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("{name}: {error}");
                worst = worst.max(2);
                continue;
            }
        };
        let golden: CompatibilityFixture = match serde_json::from_str(&text) {
            Ok(golden) => golden,
            Err(error) => {
                eprintln!("{name}: {error}");
                worst = worst.max(2);
                continue;
            }
        };
        if let Err(error) = golden.validate() {
            eprintln!("{name}: invalid compatibility matrix: {error}");
            worst = worst.max(3);
            continue;
        }
        if golden.schema_version != MATRIX_SCHEMA_VERSION {
            eprintln!("{name}: unsupported compatibility matrix schema");
            worst = worst.max(3);
            continue;
        }
        let binary = match agent::resolve(golden.agent, None) {
            Ok(binary) => binary,
            Err(error) => {
                eprintln!("{name}: cannot resolve {:?}: {error}", golden.agent);
                worst = worst.max(2);
                continue;
            }
        };
        let installed_version = match agent::version_banner(&binary.path) {
            Ok(version) => version.trim().to_owned(),
            Err(error) => {
                eprintln!("{name}: cannot read --version: {error}");
                worst = worst.max(2);
                continue;
            }
        };
        let version_match = installed_version.contains(&golden.agent_version);
        if !version_match {
            reports.push(GoldenReport {
                fixture: name,
                pinned_version: golden.version,
                installed_version,
                resolved: binary.path.display().to_string(),
                version_match,
                skipped: true,
                case_count: golden.cases.len(),
                accepted_cases: 0,
                rejected_cases: 0,
                shape_errors: Vec::new(),
                flags_checked: 0,
                passthrough_skipped: 0,
                unlisted: Vec::new(),
                restricted: Vec::new(),
                vendor_unlisted: Vec::new(),
                vendor_present: Vec::new(),
                vendor_mode_mismatch: Vec::new(),
            });
            continue;
        }
        matched_agents.push(golden.agent);
        let help = match agent_help(&binary.path) {
            Ok(help) => help,
            Err(error) => {
                eprintln!("{name}: cannot read --help: {error}");
                worst = worst.max(2);
                continue;
            }
        };
        // An empty help means the question was never really asked. Treating it
        // as "no flags missing" would certify every golden against nothing.
        if help.trim().is_empty() {
            eprintln!("{name}: {} printed no help at all", binary.path.display());
            worst = worst.max(2);
            continue;
        }
        let (accepted_cases, rejected_cases, shape_errors, emitted_flags) =
            verify_matrix_shape(&golden, &binary, &fixture_root);
        let mut unlisted = Vec::new();
        let mut restricted = Vec::new();
        let mut checked = 0;
        let mut passthrough = 0;
        for case in golden
            .cases
            .iter()
            .filter(|case| case.status == CompatibilityStatus::Accepted)
        {
            let spec = golden.expand_spec(case, &fixture_root);
            let args = golden.expand_args(case, &fixture_root);
            let used = terminalai_core::help::flags_used(&args, &spec.extra_args);
            let mapped: Vec<&str> = used
                .iter()
                .copied()
                .filter(|flag| !spec.extra_args.iter().any(|extra| extra == flag))
                .collect();
            unlisted.extend(
                mapped
                    .iter()
                    .copied()
                    .filter(|flag| !terminalai_core::help::help_lists_flag(&help, flag))
                    .map(str::to_owned),
            );
            restricted.extend(
                terminalai_core::help::mode_restricted_flags(&help, &args, &spec.extra_args)
                    .into_iter()
                    .filter(|(flag, _)| !spec.extra_args.iter().any(|extra| extra == flag))
                    .map(|(flag, requires)| GoldenRestriction {
                        flag: flag.to_owned(),
                        requires,
                    }),
            );
            checked += mapped.len();
            passthrough += spec
                .extra_args
                .iter()
                .filter(|extra| extra.starts_with('-'))
                .count();
        }
        let vendor_unlisted: Vec<String> = golden
            .vendor
            .accepted_flags
            .iter()
            .filter(|flag| !terminalai_core::help::help_lists_flag(&help, flag))
            .cloned()
            .collect();
        let vendor_present: Vec<String> = golden
            .vendor
            .unsupported_flags
            .iter()
            .filter(|flag| terminalai_core::help::help_lists_flag(&help, flag))
            .cloned()
            .collect();
        let vendor_mode_mismatch = golden
            .vendor
            .mode_restricted
            .iter()
            .filter_map(|expected| {
                let option = terminalai_core::help::help_options(&help)
                    .into_iter()
                    .find(|option| option.flags.iter().any(|flag| flag == &expected.flag));
                let actual = option
                    .as_ref()
                    .and_then(terminalai_core::help::mode_requirement);
                (actual.as_deref() != Some(expected.requires.as_str())).then_some(format!(
                    "{} requires {}, installed help says {}",
                    expected.flag,
                    expected.requires,
                    actual.as_deref().unwrap_or("no mode restriction")
                ))
            })
            .collect::<Vec<_>>();
        let mut shape_errors = shape_errors;
        for flag in &golden.vendor.accepted_flags {
            if !emitted_flags.iter().any(|emitted| emitted == flag) {
                shape_errors.push(format!(
                    "vendor accepted flag {flag} has no accepted matrix case"
                ));
            }
        }
        if !unlisted.is_empty()
            || !restricted.is_empty()
            || !vendor_unlisted.is_empty()
            || !vendor_present.is_empty()
            || !vendor_mode_mismatch.is_empty()
            || !shape_errors.is_empty()
        {
            worst = worst.max(3);
        }
        reports.push(GoldenReport {
            fixture: name,
            pinned_version: golden.version,
            installed_version,
            resolved: binary.path.display().to_string(),
            version_match,
            skipped: false,
            case_count: golden.cases.len(),
            accepted_cases,
            rejected_cases,
            shape_errors,
            flags_checked: checked,
            passthrough_skipped: passthrough,
            unlisted,
            restricted,
            vendor_unlisted,
            vendor_present,
            vendor_mode_mismatch,
        });
    }

    for agent in Agent::ALL {
        if !matched_agents.contains(&agent) {
            eprintln!(
                "no compatibility fixture matches installed {} version",
                agent.label()
            );
            worst = worst.max(3);
        }
    }

    if machine {
        match serde_json::to_string(&reports) {
            Ok(json) => println!("{json}"),
            Err(error) => return print_control_error(error, true),
        }
        return worst;
    }
    for report in &reports {
        println!("{} (pins {})", report.fixture, report.pinned_version);
        println!("  resolved: {}", report.resolved);
        println!("  installed: {}", report.installed_version);
        if report.skipped {
            println!("  SKIPPED: fixture version does not match the installed CLI");
            continue;
        }
        println!(
            "  matrix cases: {} ({} accepted, {} refused)",
            report.case_count, report.accepted_cases, report.rejected_cases
        );
        println!(
            "  flags checked: {} ({} operator passthrough skipped)",
            report.flags_checked, report.passthrough_skipped
        );
        if report.shape_errors.is_empty() {
            println!("  emitted argv shape matches every matrix case");
        } else {
            println!("  MATRIX SHAPE ERRORS: {}", report.shape_errors.join("; "));
        }
        if report.unlisted.is_empty() {
            println!("  every flag is listed by the installed CLI");
        } else {
            println!("  NOT listed by the installed CLI: {}", report.unlisted.join(", "));
        }
        if report.restricted.is_empty() {
            println!("  every flag applies in the mode this argv is in");
        } else {
            for restriction in &report.restricted {
                println!(
                    "  ACCEPTED BUT IGNORED: {} works only with {}, which this argv does not use",
                    restriction.flag, restriction.requires
                );
            }
        }
        if report.vendor_unlisted.is_empty() {
            println!("  every explicitly accepted vendor flag is listed");
        } else {
            println!(
                "  ACCEPTED FLAGS NOT LISTED: {}",
                report.vendor_unlisted.join(", ")
            );
        }
        if report.vendor_present.is_empty() {
            println!("  every explicitly unsupported vendor flag is absent");
        } else {
            println!(
                "  UNSUPPORTED FLAGS STILL LISTED: {}",
                report.vendor_present.join(", ")
            );
        }
        if report.vendor_mode_mismatch.is_empty() {
            println!("  every versioned mode restriction matches installed help");
        } else {
            println!(
                "  MODE RESTRICTION MISMATCH: {}",
                report.vendor_mode_mismatch.join("; ")
            );
        }
    }
    worst
}

fn verify_matrix_shape(
    fixture: &CompatibilityFixture,
    binary: &AgentBinary,
    root: &std::path::Path,
) -> (usize, usize, Vec<String>, Vec<String>) {
    let mut accepted = 0;
    let mut rejected = 0;
    let mut errors = Vec::new();
    let mut emitted_flags = Vec::new();
    for case in &fixture.cases {
        let spec = fixture.expand_spec(case, root);
        match case.status {
            CompatibilityStatus::Accepted => {
                accepted += 1;
                match spec.resolve(binary) {
                    Ok(command) => {
                        let expected = fixture.expand_args(case, root);
                        if command.args != expected {
                            errors.push(format!(
                                "{} expected {:?}, got {:?}",
                                case.id, expected, command.args
                            ));
                        }
                        emitted_flags.extend(
                            terminalai_core::help::flags_used(&command.args, &spec.extra_args)
                                .into_iter()
                                .map(str::to_owned),
                        );
                    }
                    Err(error) => errors.push(format!("{} refused: {error}", case.id)),
                }
            }
            CompatibilityStatus::Unsupported | CompatibilityStatus::ModeRestricted => {
                rejected += 1;
                match spec.resolve(binary) {
                    Err(error)
                        if error
                            .to_string()
                            .contains(case.error_contains.as_deref().unwrap_or_default()) => {}
                    Err(error) => errors.push(format!(
                        "{} named {}, got {error}",
                        case.id,
                        case.error_contains.as_deref().unwrap_or_default()
                    )),
                    Ok(command) => errors.push(format!(
                        "{} launched unexpectedly as {:?}",
                        case.id, command.args
                    )),
                }
            }
        }
    }
    emitted_flags.sort();
    emitted_flags.dedup();
    (accepted, rejected, errors, emitted_flags)
}

#[derive(Debug, Serialize)]
struct GoldenReport {
    fixture: String,
    pinned_version: String,
    installed_version: String,
    resolved: String,
    version_match: bool,
    skipped: bool,
    case_count: usize,
    accepted_cases: usize,
    rejected_cases: usize,
    shape_errors: Vec<String>,
    flags_checked: usize,
    passthrough_skipped: usize,
    unlisted: Vec<String>,
    /// Flags the CLI lists and would then ignore, because its own help
    /// restricts them to a mode this argv is not in.
    restricted: Vec<GoldenRestriction>,
    vendor_unlisted: Vec<String>,
    vendor_present: Vec<String>,
    vendor_mode_mismatch: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GoldenRestriction {
    flag: String,
    /// The mode flag the help says it needs.
    requires: String,
}

/// The agent's own `--help`, as the operator's shell would see it.
///
/// stdout and stderr are concatenated because CLIs disagree about where help
/// goes, and reading only one of them would silently return nothing for an
/// agent that chose the other.
fn agent_help(path: &std::path::Path) -> io::Result<String> {
    let output = std::process::Command::new(path).arg("--help").output()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}
