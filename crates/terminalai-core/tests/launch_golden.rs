use std::path::{Path, PathBuf};

use serde::Deserialize;
use terminalai_core::agent::{Agent, AgentBinary, Origin};
use terminalai_core::launch::{
    Effort, LaunchError, LaunchSpec, Permission, Resume, Sandbox,
};

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

fn claude_only(agent: Agent, values: &[&str]) -> Vec<String> {
    if agent == Agent::Claude {
        values.iter().map(|value| (*value).to_owned()).collect()
    } else {
        Vec::new()
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
        // Set deliberately, and deliberately absent from every fixture: the
        // cap is enforced by this tool's ledger, and `--max-budget-usd` only
        // binds under `--print`. This is the golden that pins that decision.
        max_budget_usd: Some(5.0),
        web_search: agent == Agent::Codex,
        // Claude-only passthrough. Codex expresses none of these — verified
        // against `codex --help` 0.146.0 — so setting them on a Codex spec
        // would refuse the launch rather than change the argv.
        allowed_tools: claude_only(agent, &["Bash(git log:*)", "Read"]),
        disallowed_tools: claude_only(agent, &["WebFetch"]),
        settings: (agent == Agent::Claude).then(|| "golden-settings.json".to_owned()),
        setting_sources: (agent == Agent::Claude).then(|| "user,project".to_owned()),
        mcp_config: claude_only(agent, &["golden-mcp.json"]),
        strict_mcp_config: agent == Agent::Claude,
        plugin_dirs: claude_only(agent, &["golden-plugin"])
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        plugin_urls: claude_only(agent, &["https://example.invalid/plugin.zip"]),
        // Neither agent expresses this: Codex has no such flag, and Claude's
        // own help restricts it to `--print`. Left out of the canonical spec
        // because setting it is now a refusal for both, which
        // `every_claude_only_option_is_refused_for_codex` asserts by name.
        fallback_model: None,
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
            // Only a path placeholder is rewritten to this platform's
            // separator. Rewriting every argument turned a URL into a UNC-ish
            // path, which is not a mismatch the fixture is trying to describe.
            if !arg.contains("__CARGO_MANIFEST_DIR__") {
                return arg;
            }
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

/// Two sessions pointed at different config directories are two accounts. The
/// variable differs per agent, so the spec names a directory and the launch
/// names the variable.
#[test]
fn the_agent_home_becomes_that_agents_own_config_variable() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (agent, key) in [
        (Agent::Claude, "CLAUDE_CONFIG_DIR"),
        (Agent::Codex, "CODEX_HOME"),
    ] {
        let spec = LaunchSpec {
            agent,
            cwd: cwd.to_path_buf(),
            agent_home: Some(cwd.to_path_buf()),
            ..Default::default()
        };
        spec.resolve(&binary(agent)).expect("resolve with an agent home");
        let environment = spec.agent_environment().expect("agent environment");
        assert!(
            environment
                .iter()
                .any(|(name, value)| name == key && value == &cwd.to_string_lossy()),
            "{agent:?} did not receive {key}: {environment:?}"
        );
    }
}

/// A directory that does not exist is refused at resolve time. Passing it on
/// would start an agent whose config lives nowhere, which reads as a signed-out
/// session rather than as a bad path.
#[test]
fn a_missing_agent_home_is_refused() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let error = LaunchSpec {
        agent: Agent::Claude,
        cwd: cwd.to_path_buf(),
        agent_home: Some(cwd.join("no-such-config-dir")),
        ..Default::default()
    }
    .resolve(&binary(Agent::Claude))
    .expect_err("a missing agent home is refused");
    assert!(matches!(error, terminalai_core::launch::LaunchError::MissingAgentHome(_)));
}

/// Nothing is inherited by being present in the parent. A named variable is the
/// operator's consent for this session, and every way that name can fail is a
/// refusal rather than a session quietly missing its credential.
#[test]
fn only_named_parent_variables_cross_and_every_bad_name_is_refused() {
    use terminalai_core::launch::LaunchError;

    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let spec_with = |names: Vec<String>| LaunchSpec {
        agent: Agent::Claude,
        cwd: cwd.to_path_buf(),
        env_passthrough: names,
        ..Default::default()
    };

    // Nothing named, nothing added — the baseline allowlist is untouched.
    assert!(spec_with(Vec::new())
        .agent_environment()
        .expect("no passthrough")
        .is_empty());

    std::env::set_var("TERMINALAI_TEST_PASSTHROUGH_KEY", "secret-value");
    let resolved = spec_with(vec!["TERMINALAI_TEST_PASSTHROUGH_KEY".into()]);
    // Reserved: the supervisor's own namespace carries the per-session hook
    // secret, so a parent value must never be able to displace one.
    assert!(matches!(
        resolved.agent_environment().expect_err("reserved"),
        LaunchError::ReservedEnvironmentName(_)
    ));

    std::env::set_var("TERMINALAI_LAUNCH_TEST_TOKEN", "secret-value");
    let allowed = LaunchSpec {
        agent: Agent::Claude,
        cwd: cwd.to_path_buf(),
        env_passthrough: vec!["PATH".into()],
        ..Default::default()
    }
    .agent_environment()
    .expect("PATH is set in every test process");
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed[0].0, "PATH");
    std::env::remove_var("TERMINALAI_TEST_PASSTHROUGH_KEY");
    std::env::remove_var("TERMINALAI_LAUNCH_TEST_TOKEN");

    assert!(matches!(
        spec_with(vec!["not a name".into()])
            .agent_environment()
            .expect_err("malformed"),
        LaunchError::InvalidEnvironmentName(_)
    ));
    assert!(matches!(
        spec_with(vec!["TERMINALAI_UNSET_FOR_THIS_TEST_ONLY_X".into()])
            .agent_environment()
            .expect_err("reserved beats unset"),
        LaunchError::ReservedEnvironmentName(_)
    ));
    assert!(matches!(
        spec_with(vec!["NO_SUCH_VARIABLE_FOR_THIS_TEST_ONLY".into()])
            .agent_environment()
            .expect_err("unset is a refusal, not an omission"),
        LaunchError::UnsetEnvironmentName(_)
    ));
}

/// Every tool, settings, MCP and plugin option is Claude-only on the versions
/// this project pins. Codex must refuse each one rather than starting a session
/// that silently lacks the constraint the operator asked for — an allowlist that
/// is quietly dropped is a session with *more* rope than was chosen, which is
/// the worst direction for that failure to go.
#[test]
fn codex_refuses_the_options_it_cannot_express() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let base = LaunchSpec {
        agent: Agent::Codex,
        cwd: cwd.to_path_buf(),
        ..Default::default()
    };
    let cases: Vec<(&str, LaunchSpec)> = vec![
        (
            "--allowed-tools",
            LaunchSpec {
                allowed_tools: vec!["Read".into()],
                ..base.clone()
            },
        ),
        (
            "--disallowed-tools",
            LaunchSpec {
                disallowed_tools: vec!["WebFetch".into()],
                ..base.clone()
            },
        ),
        (
            "--settings",
            LaunchSpec {
                settings: Some("settings.json".into()),
                ..base.clone()
            },
        ),
        (
            "--setting-sources",
            LaunchSpec {
                setting_sources: Some("user".into()),
                ..base.clone()
            },
        ),
        (
            "--mcp-config",
            LaunchSpec {
                mcp_config: vec!["mcp.json".into()],
                ..base.clone()
            },
        ),
        (
            "--strict-mcp-config",
            LaunchSpec {
                strict_mcp_config: true,
                ..base.clone()
            },
        ),
        (
            "--plugin-dir",
            LaunchSpec {
                plugin_dirs: vec![PathBuf::from("plugin")],
                ..base.clone()
            },
        ),
        (
            "--plugin-url",
            LaunchSpec {
                plugin_urls: vec!["https://example.invalid/p.zip".into()],
                ..base.clone()
            },
        ),
        (
            "--fallback-model",
            LaunchSpec {
                fallback_model: Some("gpt-5.1".into()),
                ..base.clone()
            },
        ),
    ];
    for (flag, spec) in cases {
        match spec.resolve(&binary(Agent::Codex)) {
            Err(LaunchError::Unsupported { flag: refused, .. }) => {
                assert_eq!(refused, flag, "refused the wrong option");
            }
            other => panic!("{flag} was not refused by Codex: {other:?}"),
        }
    }
}

/// These values sit next to the flags that decide what an agent may do. A value
/// that begins with a dash is not a value: it is a second option in a position
/// where `--dangerously-skip-permissions` would be accepted.
#[test]
fn option_shaped_passthrough_values_are_refused_before_argv_is_built() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let base = LaunchSpec {
        agent: Agent::Claude,
        cwd: cwd.to_path_buf(),
        ..Default::default()
    };
    let hostile = "--dangerously-skip-permissions";
    let cases: Vec<LaunchSpec> = vec![
        LaunchSpec {
            allowed_tools: vec![hostile.into()],
            ..base.clone()
        },
        LaunchSpec {
            disallowed_tools: vec![hostile.into()],
            ..base.clone()
        },
        LaunchSpec {
            mcp_config: vec![hostile.into()],
            ..base.clone()
        },
        LaunchSpec {
            settings: Some(hostile.into()),
            ..base.clone()
        },
        LaunchSpec {
            setting_sources: Some(hostile.into()),
            ..base.clone()
        },
        LaunchSpec {
            fallback_model: Some(hostile.into()),
            ..base.clone()
        },
        LaunchSpec {
            plugin_dirs: vec![PathBuf::from(hostile)],
            ..base.clone()
        },
    ];
    for spec in cases {
        assert!(
            matches!(
                spec.resolve(&binary(Agent::Claude)),
                Err(LaunchError::OptionShapedValue { .. })
            ),
            "an option-shaped value reached the command line"
        );
    }

    // A plugin URL fetches and runs remote code, so the scheme is checked too:
    // `file:` there would be a different mechanism wearing this field's name.
    for url in ["file:///C:/evil/plugin.zip", "data:application/zip;base64,UEs="] {
        assert!(matches!(
            LaunchSpec {
                plugin_urls: vec![url.into()],
                ..base.clone()
            }
            .resolve(&binary(Agent::Claude)),
            Err(LaunchError::InvalidPluginUrl(_))
        ));
    }
}
