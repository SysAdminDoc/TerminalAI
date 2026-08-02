//! Headless harness for the parts of TerminalAI that touch the machine.
//!
//! The GUI cannot be unit-tested against a real agent process, so this binary
//! carries that burden: it resolves the executables, prints the exact argument
//! vector a launcher choice produces, and drives a real ConPTY end to end.
//!
//!   terminalai-probe resolve
//!   terminalai-probe preview claude --model opus --effort xhigh --cwd .
//!   terminalai-probe spawn   codex  --raw --version
//!
//! Exit codes: 0 success, 1 usage error, 2 resolution failure, 3 spawn failure.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use terminalai_core::agent::{self, Agent};
use terminalai_core::launch::{Effort, LaunchSpec, Permission, ResolvedCommand, Sandbox};
use terminalai_core::pty::{self, PtySession};

const USAGE: &str = "\
terminalai-probe — machine-facing checks for TerminalAI

USAGE:
  terminalai-probe resolve
  terminalai-probe preview <claude|codex> [options]
  terminalai-probe spawn   <claude|codex> [options] [--raw <arg>...]

OPTIONS:
  --cwd <dir>          working directory (default: current)
  --model <name>
  --effort <low|medium|high|xhigh|max>
  --permission <ask|plan|accept-edits|bypass>
  --sandbox <read-only|workspace-write|danger-full-access>   (codex only)
  --name <label>                                             (claude only)
  --prompt <text>
  --timeout <secs>     spawn only; default 30
  --raw <arg>...       everything after this is passed through verbatim
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("resolve") => cmd_resolve(),
        Some("preview") => cmd_build(&args[1..], false),
        Some("spawn") => cmd_build(&args[1..], true),
        Some("exec") => cmd_exec(&args[1..]),
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

fn cmd_resolve() -> i32 {
    let mut failed = false;
    for a in [Agent::Claude, Agent::Codex] {
        match agent::resolve(a, None) {
            Ok(b) => println!("{:<12} {:?}  {}", a.command_name(), b.origin, b.path.display()),
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
    println!("\n--- exit: {} ---", exit.map(|c| c.to_string()).unwrap_or_else(|| "killed".into()));
    if exit == Some(0) {
        0
    } else {
        3
    }
}

fn cmd_build(args: &[String], run: bool) -> i32 {
    let Some(agent_arg) = args.first() else {
        eprintln!("missing agent\n\n{USAGE}");
        return 1;
    };
    let which = match agent_arg.as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => {
            eprintln!("unknown agent: {other}");
            return 1;
        }
    };

    let mut spec = LaunchSpec {
        agent: which,
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ..Default::default()
    };
    let mut timeout = Duration::from_secs(30);
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--cwd" => spec.cwd = PathBuf::from(value()),
            "--model" => spec.model = Some(value()),
            "--name" => spec.name = Some(value()),
            "--prompt" => spec.initial_prompt = Some(value()),
            "--timeout" => timeout = Duration::from_secs(value().parse().unwrap_or(30)),
            "--effort" => {
                spec.effort = Some(match value().as_str() {
                    "low" => Effort::Low,
                    "medium" => Effort::Medium,
                    "high" => Effort::High,
                    "xhigh" => Effort::XHigh,
                    "max" => Effort::Max,
                    other => {
                        eprintln!("bad --effort: {other}");
                        return 1;
                    }
                })
            }
            "--permission" => {
                spec.permission = Some(match value().as_str() {
                    "ask" => Permission::Ask,
                    "plan" => Permission::Plan,
                    "accept-edits" => Permission::AcceptEdits,
                    "bypass" => Permission::Bypass,
                    other => {
                        eprintln!("bad --permission: {other}");
                        return 1;
                    }
                })
            }
            "--sandbox" => {
                spec.sandbox = Some(match value().as_str() {
                    "read-only" => Sandbox::ReadOnly,
                    "workspace-write" => Sandbox::WorkspaceWrite,
                    "danger-full-access" => Sandbox::DangerFullAccess,
                    other => {
                        eprintln!("bad --sandbox: {other}");
                        return 1;
                    }
                })
            }
            "--raw" => {
                spec.extra_args.extend(it.by_ref().cloned());
                break;
            }
            other => {
                eprintln!("unknown option: {other}\n\n{USAGE}");
                return 1;
            }
        }
    }

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
    println!("\n--- exit: {} ---", exit.map(|c| c.to_string()).unwrap_or_else(|| "killed".into()));
    if exit == Some(0) {
        0
    } else {
        3
    }
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
        if let Some(code) = session.try_wait() {
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
            session.kill();
            break;
        }
    }
    (out, exit)
}
