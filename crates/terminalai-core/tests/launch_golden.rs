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

/// How often each agent's emitted approval value stops to ask, from the vendor
/// documentation rather than from this crate's opinion. Higher prompts more.
///
/// Claude: `default` asks per tool call, `acceptEdits` stops asking for file
/// edits only, `bypassPermissions` never asks.
/// Codex: `untrusted` "runs only known-safe read operations automatically" and
/// requires approval for anything that mutates state — its most prompting
/// policy, above `on-request`, which asks only on escalation. `never` asks
/// nothing.
fn prompt_frequency(agent: Agent, emitted: &str) -> u8 {
    match (agent, emitted) {
        (Agent::Claude, "default") => 3,
        (Agent::Claude, "acceptEdits") => 2,
        (Agent::Claude, "bypassPermissions") => 0,
        (Agent::Codex, "untrusted") => 4,
        (Agent::Codex, "on-request") => 3,
        (Agent::Codex, "never") => 0,
        other => panic!("unranked approval value {other:?}; rank it against the vendor docs"),
    }
}

/// The emitted approval value for one permission, or `None` when the agent
/// expresses that permission some other way than an approval flag.
fn emitted_approval(agent: Agent, permission: Permission) -> Option<String> {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let spec = LaunchSpec {
        agent,
        cwd: cwd.to_path_buf(),
        permission: Some(permission),
        ..Default::default()
    };
    let args = spec
        .resolve(&binary(agent))
        .expect("resolve permission-only launch")
        .args;
    let flag = match agent {
        Agent::Claude => "--permission-mode",
        Agent::Codex => "--ask-for-approval",
    };
    args.iter()
        .position(|arg| arg == flag)
        .map(|at| args[at + 1].clone())
}

/// Asking for *less* interruption must never produce *more* of it. This is the
/// one property the per-agent mapping table cannot state about itself, and the
/// Codex column violated it: `AcceptEdits` mapped to `untrusted`, which asks
/// more than the `on-request` that `Ask` maps to.
#[test]
fn asking_for_less_interruption_never_produces_more_of_it() {
    for agent in [Agent::Claude, Agent::Codex] {
        let ladder = [
            Permission::Ask,
            Permission::AcceptEdits,
            Permission::Bypass,
        ];
        let mut previous: Option<(Permission, u8)> = None;
        for permission in ladder {
            let Some(emitted) = emitted_approval(agent, permission.clone()) else {
                continue;
            };
            let rank = prompt_frequency(agent, &emitted);
            if let Some((looser, earlier)) = previous.as_ref() {
                assert!(
                    *earlier >= rank,
                    "{agent:?}: {permission:?} emits {emitted:?} which prompts more than \
                     {looser:?} does — the ladder is inverted"
                );
            }
            previous = Some((permission, rank));
        }
    }
}

/// Claude Code has grown `auto`, `dontAsk` and `manual` since this crate's four
/// modes were written. A closed enum would not merely fail to offer them — it
/// would silently rewrite a stored preset that names one.
#[test]
fn a_permission_mode_this_build_does_not_model_reaches_the_agent() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (agent, flag) in [
        (Agent::Claude, "--permission-mode"),
        (Agent::Codex, "--ask-for-approval"),
    ] {
        let args = LaunchSpec {
            agent,
            cwd: cwd.to_path_buf(),
            permission: Some(Permission::parse("dontAsk")),
            ..Default::default()
        }
        .resolve(&binary(agent))
        .expect("an unmodelled mode is passed through, not refused")
        .args;
        let at = args
            .iter()
            .position(|arg| arg == flag)
            .unwrap_or_else(|| panic!("{agent:?} emits {flag}"));
        assert_eq!(args[at + 1], "dontAsk");
    }
}

/// The four modelled modes must keep the spelling stored presets and repository
/// templates already use, or every saved preset silently becomes a custom one.
#[test]
fn the_modelled_permission_names_round_trip_through_serde() {
    for permission in [
        Permission::Ask,
        Permission::Plan,
        Permission::AcceptEdits,
        Permission::Bypass,
        Permission::Custom("dontAsk".into()),
    ] {
        let encoded = serde_json::to_string(&permission).expect("encode permission");
        let decoded: Permission = serde_json::from_str(&encoded).expect("decode permission");
        assert_eq!(decoded, permission, "round trip changed {permission:?}");
    }
    assert_eq!(
        serde_json::to_string(&Permission::AcceptEdits).expect("encode"),
        "\"accept-edits\"",
        "the stored spelling changed; every saved preset would become custom"
    );
    assert!(!Permission::Bypass.is_custom());
    assert!(Permission::parse("manual").is_custom());
}

/// `acceptEdits` means "make the edits without asking". A sandbox that forbids
/// writing makes that impossible, so the pair is refused rather than launched
/// into a session that will fail on its first edit.
#[test]
fn accept_edits_under_a_read_only_sandbox_is_refused() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let error = LaunchSpec {
        agent: Agent::Codex,
        cwd: cwd.to_path_buf(),
        permission: Some(Permission::AcceptEdits),
        sandbox: Some(Sandbox::ReadOnly),
        ..Default::default()
    }
    .resolve(&binary(Agent::Codex))
    .expect_err("a read-only sandbox cannot accept edits");
    let message = error.to_string();
    assert!(
        message.contains("accept-edits") && message.contains("read-only"),
        "refusal must name both halves of the contradiction: {message}"
    );
}

/// Without an explicit sandbox, accept-edits pairs with the workspace-write
/// sandbox — Codex's own documented auto preset. Codex's default sandbox is not
/// guaranteed to permit writes, so leaving it unset would ask the agent to
/// accept edits it cannot make.
#[test]
fn accept_edits_pairs_with_the_workspace_write_sandbox_by_default() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let args = LaunchSpec {
        agent: Agent::Codex,
        cwd: cwd.to_path_buf(),
        permission: Some(Permission::AcceptEdits),
        ..Default::default()
    }
    .resolve(&binary(Agent::Codex))
    .expect("resolve accept-edits launch")
    .args;
    let at = args
        .iter()
        .position(|arg| arg == "--sandbox")
        .expect("accept-edits supplies a sandbox");
    assert_eq!(args[at + 1], "workspace-write");
}
