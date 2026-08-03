use std::path::{Path, PathBuf};

use serde::Deserialize;
use terminalai_core::agent::{Agent, AgentBinary, Origin};
use terminalai_core::launch::{Effort, LaunchSpec, Permission, Resume, Sandbox};

#[derive(Debug, Deserialize)]
struct Golden {
    version: String,
    expected_args: Vec<String>,
}

fn binary(agent: Agent) -> AgentBinary {
    AgentBinary {
        agent,
        path: PathBuf::from("terminalai-agent"),
        origin: Origin::Configured,
    }
}

fn canonical_spec(agent: Agent, cwd: &Path) -> LaunchSpec {
    LaunchSpec {
        agent,
        name: Some("golden review".into()),
        cwd: cwd.to_path_buf(),
        model: Some(if agent == Agent::Claude {
            "opus".into()
        } else {
            "gpt-5.1-codex".into()
        }),
        effort: Some(Effort::XHigh),
        permission: Some(if agent == Agent::Claude {
            Permission::Plan
        } else {
            Permission::AcceptEdits
        }),
        sandbox: (agent == Agent::Codex).then_some(Sandbox::WorkspaceWrite),
        profile: (agent == Agent::Codex).then_some("work".into()),
        add_dirs: vec![cwd.join("shared")],
        resume: if agent == Agent::Claude {
            Resume::Fork("claude-session-1".into())
        } else {
            Resume::Session("codex-thread-1".into())
        },
        max_budget_usd: (agent == Agent::Claude).then_some(5.0),
        web_search: agent == Agent::Codex,
        initial_prompt: Some("--dangerously-skip-permissions".into()),
        extra_args: vec!["--verbose".into()],
        ..Default::default()
    }
}

fn assert_golden(agent: Agent, fixture: &str) {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let golden: Golden = serde_json::from_str(fixture).expect("decode launch golden");
    assert!(!golden.version.is_empty(), "fixture must pin a CLI version");
    let command = canonical_spec(agent, cwd)
        .resolve(&binary(agent))
        .expect("resolve canonical launch");
    let expected = golden
        .expected_args
        .into_iter()
        .map(|arg| {
            let separator = std::path::MAIN_SEPARATOR.to_string();
            arg.replace("__CARGO_MANIFEST_DIR__", &cwd.to_string_lossy())
                .replace('/', &separator)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        command.args, expected,
        "{} argument vector changed",
        golden.version
    );
}

#[test]
fn claude_code_2_1_170_arguments_match_golden() {
    assert_golden(
        Agent::Claude,
        include_str!("fixtures/launch/claude-code-2.1.170.json"),
    );
}

#[test]
fn codex_cli_0_146_0_arguments_match_golden() {
    assert_golden(
        Agent::Codex,
        include_str!("fixtures/launch/codex-cli-0.146.0.json"),
    );
}
