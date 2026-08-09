//! Short-lived agent-hook adapter.
//!
//! Hook delivery deliberately bypasses Tauri/WebView2. It reads the agent's
//! event from stdin, sends a bounded daemon request, and always returns the
//! agent's command path without making the desktop shell a dependency.

use std::io::{self, Read};
use std::time::Duration;

use terminalai_core::agent::Agent;
use terminalai_core::parse_hook_in;
use terminalai_daemon::{DaemonClient, Request, Response};

pub(crate) fn is_hook_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == "hook")
}

/// Deliver an agent hook without initializing Tauri/WebView2.
///
/// Hook commands are deliberately fail-open: an unavailable desktop daemon
/// must never stall or fail the user's agent command. Claude's async hook
/// support normally keeps this off the agent's critical path; the short
/// timeout also bounds Codex's synchronous command hook.
pub(crate) fn run_hook_cli(args: &[String]) -> i32 {
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
    match client.call_with_timeout(Request::Hook { event, hook_token }, timeout) {
        Ok(Response::Hook { .. }) | Ok(Response::Ok) => {}
        Ok(other) => eprintln!("ignoring unexpected hook response: {other:?}"),
        Err(error) => eprintln!("ignoring hook delivery failure: {error}"),
    }
    0
}
