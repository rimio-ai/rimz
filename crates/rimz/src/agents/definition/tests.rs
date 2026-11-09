use serde_json::json;

use super::{CompactCommand, CompactInstruction, LaunchSpec, PromptStyle, program_names_kind};
use crate::agents::registry::BUILTINS;
use crate::agents::{AskKind, ToolEffect};

#[test]
fn permission_settlement_defaults_and_maps_each_kind() {
    use super::PermissionMode::{Ask, Auto, Plan, Yolo};
    for (kind, default, mapped) in [
        ("claude", Some(Auto), [Plan, Ask, Auto, Yolo]),
        ("codex", Some(Auto), [Plan, Ask, Auto, Yolo]),
        ("opencode", Some(Auto), [Auto, Auto, Auto, Yolo]),
        ("pi", Some(Yolo), [Yolo; 4]),
        ("amp", None, [Plan, Ask, Auto, Yolo]),
    ] {
        let launch = crate::agents::spec_by_kind(kind).unwrap().launch;
        assert_eq!(
            super::LaunchPermissionArgs::of_kind(kind).resolve_mode(None),
            default,
            "{kind} permission table"
        );
        let mut args = vec!["--unrelated".to_owned()];
        assert_eq!(
            launch.settle_permission_mode(&mut args, None, None),
            default,
            "{kind} default"
        );
        let mut expected = vec!["--unrelated".to_owned()];
        if let Some(mode) = default {
            expected.extend(launch.permission_args(mode));
        }
        assert_eq!(args, expected, "{kind} default argv");
        for (requested, mapped) in [Plan, Ask, Auto, Yolo].into_iter().zip(mapped) {
            let mut args = launch.permission_args(requested);
            assert_eq!(
                launch.settle_permission_mode(&mut args, Some(requested), Some(Yolo)),
                Some(mapped),
                "{kind} {requested}"
            );
            assert_eq!(args, launch.permission_args(mapped), "{kind} {requested}");
        }
    }
    for requested in [None, Some(Plan), Some(Ask), Some(Auto), Some(Yolo)] {
        assert_eq!(
            super::LaunchPermissionArgs::of_kind("missing-kind").resolve_mode(requested),
            requested
        );
    }
}

#[test]
fn permission_settlement_yields_defaults_to_raw_markers_and_records_unknown() {
    use super::PermissionMode::{Auto, Yolo};
    for (kind, raw) in [
        ("claude", vec!["--permission-mode", "plan"]),
        ("claude", vec!["--permission-mode=plan"]),
        ("claude", vec!["--dangerously-skip-permissions"]),
        ("codex", vec!["--ask-for-approval", "never"]),
        ("codex", vec!["--ask-for-approval=never"]),
        ("codex", vec!["-a", "never"]),
        ("codex", vec!["-anever"]),
        ("codex", vec!["-c", "approvals_reviewer=user"]),
        ("codex", vec!["--config=approvals_reviewer=user"]),
        ("codex", vec!["-c", "approval_policy=never"]),
        ("codex", vec!["--dangerously-bypass-approvals-and-sandbox"]),
        ("codex", vec!["--yolo"]),
        ("codex", vec!["--approve-for-me"]),
        ("codex", vec!["--not-so-yolo"]),
        ("opencode", vec!["--auto"]),
        ("opencode", vec!["--yolo"]),
        ("opencode", vec!["--dangerously-skip-permissions"]),
    ] {
        let launch = crate::agents::spec_by_kind(kind).unwrap().launch;
        let raw: Vec<String> = raw.into_iter().map(str::to_owned).collect();
        for default in [None, Some(Auto), Some(Yolo)] {
            let mut args = raw.clone();
            assert_eq!(
                launch.settle_permission_mode(&mut args, None, default),
                None,
                "{kind} raw {raw:?}"
            );
            assert_eq!(args, raw, "{kind}: a default must not override raw flags");
        }
        let mut args = launch.permission_args(Auto);
        args.extend(raw.iter().cloned());
        let expected = args.clone();
        assert_eq!(
            launch.settle_permission_mode(&mut args, Some(Auto), None),
            None,
            "{kind} requested auto plus raw {raw:?}"
        );
        assert_eq!(args, expected);
    }
}

#[test]
fn permission_settlement_ignores_only_one_owned_vector_and_exact_flag_names() {
    use super::PermissionMode::Auto;
    for kind in ["claude", "codex"] {
        let launch = crate::agents::spec_by_kind(kind).unwrap().launch;
        let mut args = launch.permission_args(Auto);
        args.extend(launch.permission_args(Auto));
        assert_eq!(
            launch.settle_permission_mode(&mut args, Some(Auto), None),
            None,
            "{kind} repeated vector is unknown"
        );
    }
    for (kind, raw) in [
        ("claude", "--permission-mode-extra"),
        ("codex", "--ask-for-approval-extra"),
        ("codex", "-b"),
        ("codex", "--a"),
        ("opencode", "--automatic"),
    ] {
        let launch = crate::agents::spec_by_kind(kind).unwrap().launch;
        let mut args = vec![raw.to_owned()];
        assert_eq!(
            launch.settle_permission_mode(&mut args, None, None),
            Some(Auto),
            "{kind} {raw} is not a permission flag"
        );
    }
}

#[test]
fn plan_labels_use_named_tiers_with_title_case_fallbacks() {
    for (kind, raw, expected) in [
        ("codex", "prolite", "ChatGPT Pro 100"),
        ("codex", "pro", "ChatGPT Pro 200"),
        ("codex", "promax", "ChatGPT Pro 500"),
        ("codex", "plus", "ChatGPT Plus"),
        ("codex", "team", "ChatGPT Team"),
        ("codex", "future_tier", "ChatGPT Future Tier"),
        ("codex", "Pro", "ChatGPT Pro"),
        ("claude", "max", "Claude Max"),
        ("claude", "max_5x", "Claude Max 5x"),
        ("claude", "max_20x", "Claude Max 20x"),
        ("copilot", "pro", "Copilot Pro"),
        ("pi", "pro", "Pro"),
    ] {
        let spec = crate::agents::spec_by_kind(kind).unwrap();
        assert_eq!(spec.plan_label.format(raw, None), expected, "{kind}: {raw}");
    }
}

#[test]
fn plan_capacities_resolve_exact_raw_tiers_to_keys_and_units() {
    let resolve = |kind: &str, tier: &str| {
        let capacities = crate::agents::spec_by_kind(kind)?
            .plan_capacities
            .as_ref()?;
        let plan = capacities.resolve(tier)?;
        Some((plan.key, plan.units, capacities.unit))
    };
    for (kind, tier, key, units) in [
        ("claude", "pro", "pro", 100),
        ("claude", "team_standard", "team_standard", 125),
        ("claude", "team_premium", "team_premium", 625),
        ("claude", "max_5x", "max", 500),
        ("claude", "max_20x", "ultra", 1000),
        ("codex", "plus", "plus", 100),
        ("codex", "team", "team_standard", 100),
        ("codex", "business_standard", "team_standard", 100),
        ("codex", "premium", "team_premium", 1000),
        ("codex", "self_serve_business_prolite", "team_premium", 1000),
        ("codex", "business_premium", "team_premium", 1000),
        ("codex", "prolite", "pro_100", 1000),
        ("codex", "pro", "pro_200", 2000),
        ("codex", "promax", "pro_500", 5000),
    ] {
        let unit = if kind == "claude" {
            "claude_pro_week_percent"
        } else {
            "chatgpt_plus_week_percent"
        };
        assert_eq!(
            resolve(kind, tier),
            Some((key, units, unit)),
            "{kind}: {tier}"
        );
    }
    for (kind, tier) in [
        ("claude", "max"),
        ("claude", "team"),
        ("claude", "enterprise"),
        ("claude", "Pro"),
        ("codex", "free"),
        ("codex", "business"),
        ("copilot", "pro"),
        ("unregistered", "pro"),
    ] {
        assert_eq!(resolve(kind, tier), None, "{kind}: {tier}");
    }
}

#[test]
fn plan_labels_prefer_the_profile_name_without_changing_its_case() {
    let actual: Vec<_> = ["claude", "codex", "pi"]
        .into_iter()
        .map(|kind| {
            crate::agents::spec_by_kind(kind)
                .unwrap()
                .plan_label
                .format("pro", Some("ultra"))
        })
        .collect();
    assert_eq!(actual, ["Claude ultra", "ChatGPT ultra", "ultra"]);
}

#[test]
fn context_owner_start_requires_a_verified_live_owner() {
    let spec = crate::agents::spec_by_kind("claude").unwrap();
    let started = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    for raw in [None, Some("bad"), Some("0"), Some("1"), Some("4294967296")] {
        assert_eq!(
            super::context_owner_start_with(
                spec,
                raw,
                |_| panic!("invalid pid reached comm"),
                |_| panic!("invalid pid reached start")
            ),
            None
        );
    }
    assert_eq!(
        super::context_owner_start_with(
            spec,
            Some("42"),
            |_| Some("zsh".to_owned()),
            |_| panic!("wrong owner reached start")
        ),
        None
    );
    assert_eq!(
        super::context_owner_start_with(
            spec,
            Some("42"),
            |_| None,
            |_| panic!("unknown owner reached start")
        ),
        None
    );
    assert_eq!(
        super::context_owner_start_with(spec, Some("42"), |_| Some("claude".to_owned()), |_| None),
        None
    );
    let mut reads = 0;
    assert_eq!(
        super::context_owner_start_with(
            spec,
            Some("42"),
            |_| {
                reads += 1;
                Some(if reads == 1 { "claude" } else { "zsh" }.to_owned())
            },
            |_| Some(started)
        ),
        None
    );
    for name in ["claude", "claude-aarch64-unknown-linux-gnu"] {
        assert_eq!(
            super::context_owner_start_with(
                spec,
                Some("42"),
                |pid| {
                    assert_eq!(pid, 42);
                    Some(name.to_owned())
                },
                |pid| {
                    assert_eq!(pid, 42);
                    Some(started)
                }
            ),
            Some(started)
        );
    }
}

#[cfg(unix)]
#[test]
fn hook_owner_env_restores_only_a_declared_verified_owner_once() {
    use super::{HOOK_ENV_RESTORED, hook_owner_env, hook_owner_env_with};
    use crate::agents::capabilities::{CoreCapability, HookCapability};

    struct ScrubbedHook;
    impl CoreCapability for ScrubbedHook {
        fn spec(&self) -> &'static super::AgentSpec {
            crate::agents::spec_by_kind("codex").unwrap()
        }
    }
    impl HookCapability for ScrubbedHook {
        fn hook_inherits_env(&self) -> bool {
            false
        }
    }
    struct InheritingHook;
    impl CoreCapability for InheritingHook {
        fn spec(&self) -> &'static super::AgentSpec {
            ScrubbedHook.spec()
        }
    }
    impl HookCapability for InheritingHook {}

    let owner_env = || {
        [
            ("RIMZ_HOME", "/isolated/rimz"),
            ("XDG_RUNTIME_DIR", "/isolated/runtime"),
            ("RIMZ_PROJECT_ROOT", "/project"),
            ("RIMZ_WORKSPACE_ID", "workspace"),
            ("RIMZ_RUN_ID", "run"),
            ("TMUX_PANE", "%9"),
            ("RIMZ_AGENT_PID", "stale"),
            (HOOK_ENV_RESTORED, "stale"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .to_vec()
    };
    for comm in ["codex", "node", "codex-aarch64-unknown-linux-gnu"] {
        let restored = hook_owner_env_with(
            &ScrubbedHook,
            Some("42"),
            false,
            |pid| {
                assert_eq!(pid, 42);
                Some(comm.to_owned())
            },
            |pid| {
                assert_eq!(pid, 42);
                Some(owner_env())
            },
        )
        .unwrap();
        assert!(
            restored.is_some(),
            "a declaring hook must restore its owner env"
        );
        let restored = restored.unwrap();
        let mut expected = owner_env();
        expected.retain(|(key, _)| key != "RIMZ_AGENT_PID" && key != HOOK_ENV_RESTORED);
        expected.push(("RIMZ_AGENT_PID".to_owned(), "42".to_owned()));
        expected.push((HOOK_ENV_RESTORED.to_owned(), "1".to_owned()));
        assert_eq!(restored, expected);
    }

    assert!(hook_owner_env("unregistered").unwrap().is_none());
    assert!(
        hook_owner_env_with(
            &InheritingHook,
            Some("42"),
            false,
            |_| panic!("inheriting hooks must not probe an owner"),
            |_| panic!("inheriting hooks must not read an owner env"),
        )
        .unwrap()
        .is_none()
    );
    assert!(
        hook_owner_env_with(
            &ScrubbedHook,
            Some("42"),
            true,
            |_| panic!("restored hooks must not probe again"),
            |_| panic!("restored hooks must not read an owner env"),
        )
        .unwrap()
        .is_none()
    );
    for pid in [None, Some("bad"), Some("0"), Some("1")] {
        assert!(
            hook_owner_env_with(
                &ScrubbedHook,
                pid,
                false,
                |_| panic!("invalid pids must not be probed"),
                |_| panic!("invalid pids must not read an owner env"),
            )
            .is_err()
        );
    }
    for comm in [None, Some("zsh".to_owned())] {
        assert!(
            hook_owner_env_with(
                &ScrubbedHook,
                Some("42"),
                false,
                |_| comm.clone(),
                |_| { panic!("unverified owners must not supply an env") }
            )
            .is_err()
        );
    }
    assert!(
        hook_owner_env_with(
            &ScrubbedHook,
            Some("42"),
            false,
            |_| Some("codex".to_owned()),
            |_| None,
        )
        .is_err()
    );
    let mut probes = 0;
    assert!(
        hook_owner_env_with(
            &ScrubbedHook,
            Some("42"),
            false,
            |_| {
                probes += 1;
                Some(if probes == 1 { "codex" } else { "zsh" }.to_owned())
            },
            |_| Some(owner_env()),
        )
        .is_err(),
        "an owner that changed during the env read must not be restored"
    );
}

#[test]
fn every_descriptor_keeps_editing_a_subset_of_mutating() {
    for adapter in BUILTINS {
        let definition = adapter.spec();
        for tool in definition.tools.editing {
            assert!(
                definition.tools.mutating.contains(tool),
                "{}: editing tool {tool} missing from the mutating set",
                definition.kind,
            );
        }
    }
}

#[test]
fn joined_prompt_preserves_leading_dash_and_spaces() {
    let launch = LaunchSpec {
        program: Some("agent"),
        fixed_args: &["chat"],
        prompt: PromptStyle::JoinedFlag("--query"),
        ..LaunchSpec::EMPTY
    };
    assert_eq!(
        launch.launch_command(&["--cli".into()], Some("-x test")),
        Some(vec![
            "agent".into(),
            "chat".into(),
            "--cli".into(),
            "--query=-x test".into()
        ])
    );
    for prompt in [None, Some("")] {
        assert_eq!(
            launch.launch_command(&[], prompt),
            Some(vec!["agent".into(), "chat".into()])
        );
    }
}

#[test]
fn prompt_flag_suffix_is_conditional_on_nonempty_prompt() {
    let launch = LaunchSpec {
        program: Some("agent"),
        fixed_args: &["--fixed"],
        prompt: PromptStyle::FlagWithSuffix {
            flag: "--prompt",
            suffix: &["--format", "jsonl"],
        },
        ..LaunchSpec::EMPTY
    };
    let extra = ["--verbose".to_owned()];
    assert_eq!(
        launch.launch_command(&extra, Some("review")),
        Some(
            [
                "agent",
                "--fixed",
                "--verbose",
                "--prompt",
                "review",
                "--format",
                "jsonl",
            ]
            .map(ToOwned::to_owned)
            .to_vec()
        )
    );
    for prompt in [None, Some("")] {
        assert_eq!(
            launch.launch_command(&extra, prompt),
            Some(
                ["agent", "--fixed", "--verbose"]
                    .map(ToOwned::to_owned)
                    .to_vec()
            )
        );
    }
}

#[test]
fn compact_command_renders_supported_instructions_on_one_line() {
    let trailing = LaunchSpec {
        compact_command: Some(CompactCommand {
            command: "/compact",
            instruction: CompactInstruction::Trailing,
        }),
        ..LaunchSpec::EMPTY
    };
    assert_eq!(
        trailing.compact_command("a\n  b\tc "),
        Some("/compact a b c".to_owned())
    );
    for instruction in ["", " \n\t "] {
        assert_eq!(
            trailing.compact_command(instruction),
            Some("/compact".to_owned())
        );
    }

    let unsupported = LaunchSpec {
        compact_command: Some(CompactCommand {
            command: "/compact",
            instruction: CompactInstruction::Unsupported,
        }),
        ..LaunchSpec::EMPTY
    };
    assert_eq!(
        unsupported.compact_command("keep the plan"),
        Some("/compact".to_owned())
    );
    assert_eq!(LaunchSpec::EMPTY.compact_command("instruction"), None);
}

#[test]
fn descriptor_classifies_mutating_editing_and_blocking_tools() {
    let claude = crate::agents::registry::spec_by_kind("claude").unwrap();
    let effect = |name| claude.tool_effect(&json!({ "tool_name": name }));
    assert_eq!(effect("Edit"), ToolEffect::Editing);
    // Command runners mutate but do not edit — the reasoning phase survives.
    assert_eq!(effect("Bash"), ToolEffect::Mutating);
    assert_eq!(effect("Read"), ToolEffect::Observed);
    assert_eq!(claude.tool_effect(&json!({})), ToolEffect::Observed);
    assert_eq!(effect("Write"), ToolEffect::Editing);
    assert_eq!(
        claude.blocking_tool_kind(Some("ExitPlanMode")),
        Some(AskKind::PlanApproval)
    );
    assert_eq!(
        claude.blocking_tool_kind(Some("AskUserQuestion")),
        Some(AskKind::Question)
    );
    assert_eq!(claude.blocking_tool_kind(Some("request_user_input")), None);

    let codex = crate::agents::registry::spec_by_kind("codex").unwrap();
    assert_eq!(
        codex.tool_effect(&json!({ "tool_name": "apply_patch" })),
        ToolEffect::Editing
    );
    assert_eq!(
        codex.tool_effect(&json!({ "tool_name": "shell" })),
        ToolEffect::Mutating
    );
    assert_eq!(
        codex.blocking_tool_kind(Some("request_user_input")),
        Some(AskKind::Question)
    );
    assert_eq!(codex.blocking_tool_kind(Some("ExitPlanMode")), None);
    assert_eq!(codex.blocking_tool_kind(Some("update_plan")), None);
    assert_eq!(codex.blocking_tool_kind(None), None);
}

#[test]
fn tool_signature_is_canonical_and_argument_sensitive() {
    let claude = crate::agents::registry::spec_by_kind("claude").unwrap();
    let ordered = claude
        .tool_signature(&json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cargo check", "timeout": 30 }
        }))
        .expect("Claude signature");
    let reordered = claude
        .tool_signature(&json!({
            "tool_input": { "timeout": 30, "command": "cargo check" },
            "tool_name": "Bash"
        }))
        .expect("reordered Claude signature");
    let changed = claude
        .tool_signature(&json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test", "timeout": 30 }
        }))
        .expect("changed Claude signature");

    assert_eq!(ordered, reordered);
    assert_ne!(ordered, changed);
}

#[test]
fn tool_signature_covers_each_reachable_adapter() {
    for (kind, payload, expected) in [
        (
            "claude",
            json!({
                "tool_name": "Bash",
                "tool_input": {"command": "cargo check", "timeout": 30}
            }),
            "b5b6f4d3b2a47915",
        ),
        (
            "codex",
            json!({"tool_name": "exec_command", "tool_input": {"cmd": "cargo check"}}),
            "934fb6c158c4e4bb",
        ),
    ] {
        let spec = crate::agents::registry::spec_by_kind(kind).unwrap();
        assert_eq!(
            spec.tool_signature(&payload).as_deref(),
            Some(expected),
            "{kind} hook payload"
        );
    }

    let opencode = crate::agents::registry::spec_by_kind("opencode").unwrap();
    assert_eq!(
        opencode.tool_signature(&json!({
            "tool_name": "bash",
            "input": {"command": "cargo check"}
        })),
        None
    );
}

#[test]
fn tool_signature_rejects_empty_names() {
    let claude = crate::agents::registry::spec_by_kind("claude").unwrap();
    for name in ["", "   "] {
        assert_eq!(
            claude.tool_signature(&json!({
                "tool_name": name,
                "tool_input": {"command": "cargo check"}
            })),
            None
        );
    }
}

#[test]
fn target_triple_binary_names_still_name_the_agent_kind() {
    for name in [
        "codex",
        "codex-aarch64-apple-darwin",
        "codex-x86_64-apple-darwin",
        "codex-x86_64-unknown-linux-musl",
        "codex-aarch64-unknown-linux-gnu",
        "codex-aarch64-a",
    ] {
        assert!(program_names_kind(name, "codex"), "{name}");
    }

    for name in ["codexfoo", "codex-plan", "codex-appserver-stub", "node"] {
        assert!(!program_names_kind(name, "codex"), "{name}");
    }
}

#[test]
fn descriptor_run_names_include_launchers_and_target_triples() {
    let codex = crate::agents::registry::spec_by_kind("codex").unwrap();

    assert!(codex.runs_as("codex"));
    assert!(codex.runs_as("node"));
    assert!(codex.runs_as("codex-aarch64-a"));
    assert!(!codex.runs_as("zsh"));
}
