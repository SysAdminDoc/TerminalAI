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
use std::sync::mpsc;
use std::time::Duration;
use std::{io, io::Read};

use terminalai_core::agent::{self, Agent};
use terminalai_core::launch::{Effort, LaunchSpec, Permission, ResolvedCommand, Sandbox};
use terminalai_core::pty::{self, PtySession};
use terminalai_core::{parse_hook, HookTransport};
use terminalai_daemon::{DaemonClient, HookEndpoint, Request, Response};

const USAGE: &str = "\
terminalai-probe — machine-facing checks for TerminalAI

USAGE:
  terminalai-probe resolve
  terminalai-probe capabilities <claude|codex> [--json]
  terminalai-probe preview <claude|codex> [options]
  terminalai-probe spawn   <claude|codex> [options] [--raw <arg>...]
  terminalai-probe list    --json
  terminalai-probe start   <claude|codex> [options] --json
  terminalai-probe stop    <session-id> --json
  terminalai-probe send    <session-id> <text> --json
  terminalai-probe status  <session-id> --json
  terminalai-probe shutdown
  terminalai-probe hook    <claude|codex>    (read one hook JSON object from stdin)
  terminalai-probe hooks   <status|preview|install|remove> <claude|codex> [options]
  terminalai-probe cpu-idle [--sessions <n>] [--seconds <s>] [--poll]

OPTIONS:
  --cwd <dir>          working directory (default: current)
  --model <name>
  --effort <runtime value>
  --permission <ask|plan|accept-edits|bypass>
  --sandbox <read-only|workspace-write|danger-full-access>   (codex only)
  --name <label>                                             (claude only)
  --prompt <text>
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("resolve") => cmd_resolve(),
        Some("capabilities") => cmd_capabilities(&args[1..]),
        Some("preview") => cmd_build(&args[1..], false),
        Some("spawn") => cmd_build(&args[1..], true),
        Some("list") => cmd_list(&args[1..]),
        Some("start") => cmd_start(&args[1..]),
        Some("stop") => cmd_stop(&args[1..]),
        Some("send") => cmd_send(&args[1..]),
        Some("status") => cmd_status(&args[1..]),
        Some("shutdown") => cmd_shutdown(&args[1..]),
        Some("exec") => cmd_exec(&args[1..]),
        Some("cpu-idle") => cmd_cpu_idle(&args[1..]),
        Some("hook") => cmd_hook(&args[1..]),
        Some("hooks") => cmd_hooks(&args[1..]),
        Some("--help") | Some("-h") | None => {
            print!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            1
        }
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
    eprintln!("{message}\n\n{USAGE}");
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
    let event = match parse_hook(agent, &input) {
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
    match client.call_with_timeout(Request::Hook { event }, timeout) {
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
        watchers.push(std::thread::spawn(move || {
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
        }));
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

fn cmd_build(args: &[String], run: bool) -> i32 {
    let (spec, timeout) = match parse_launch_spec(args, true) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
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
                spec.permission = Some(match take_value(args, &mut index, flag)?.as_str() {
                    "ask" => Permission::Ask,
                    "plan" => Permission::Plan,
                    "accept-edits" => Permission::AcceptEdits,
                    "bypass" => Permission::Bypass,
                    other => return Err(format!("bad --permission: {other}")),
                });
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
    use terminalai_core::{HookNotification, HookSignal};

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
