use super::launch::*;
use super::*;
use clap::{CommandFactory, Parser};
use jiff::Timestamp;
use rimz::agents::PermissionMode;
use rimz::agents::{
    AgentState, AgentStatus, AgentTurnError, LaunchPreset, TurnErrorClass, TurnPhase,
};
use rimz::config::{MachineConfig, Profile, ProfilesConfig, ThemeConfig, ThemeGlyphsConfig};
use rimz::forge::Forge;
use rimz::harness::plan::LaunchFinalizeOptions;
use rimz::ids::{AgentKind, AgentSessionId, MessageId, MuxName, PaneId, RunId, WorkspaceId};
use rimz::store::run::{RunRecord, RunStatus};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Parser)]
struct AgentsHarness {
    #[command(flatten)]
    args: AgentsArgs,
}

#[test]
fn logs_last_and_tail_are_aliases() {
    for flag in ["--last", "--tail"] {
        let parsed = AgentsHarness::try_parse_from(["agents", "logs", "@coder", flag, "2"]);
        assert!(parsed.is_ok(), "{parsed:?}");
        assert!(matches!(
            parsed.unwrap().args.command,
            Some(AgentsSubcmd::Logs { tail: Some(2), .. })
        ));
    }
}

#[test]
fn create_from_worktree_channel_launches_in_place() {
    let args = create_args_for_current_channel("/repo-worktrees/feat-x", None, None);
    assert_eq!(args.launch.cohort.worktree, None);
    assert_eq!(args.launch.cohort.channel, None);
}

#[test]
fn create_from_main_checkout_derived_card_launches_in_place() {
    let mut agent = rimz::testkit::agent_state("claude", "caller", Timestamp::UNIX_EPOCH);
    agent.worktree_path = Some("/repo".to_owned());
    let args = create_args_for_current_channel("/repo", None, Some(agent));
    assert_eq!(args.launch.cohort.worktree, None);
    assert_eq!(args.launch.cohort.channel, None);
}

#[test]
fn create_from_named_channel_preserves_the_lane() {
    let mut agent = rimz::testkit::agent_state("claude", "caller", Timestamp::UNIX_EPOCH);
    agent.worktree_path = Some("/repo".to_owned());
    agent.channel = Some("team-lane".to_owned());
    let args = create_args_for_current_channel("/repo", None, Some(agent));
    assert_eq!(args.launch.cohort.worktree, None);
    assert_eq!(args.launch.cohort.channel.as_deref(), Some("team-lane"));
    let args =
        create_args_for_current_channel("/repo-worktrees/feat-x", Some("explicit-lane"), None);
    assert_eq!(args.launch.cohort.worktree, None);
    assert_eq!(args.launch.cohort.channel.as_deref(), Some("explicit-lane"));
}

fn create_args_for_current_channel(
    worktree: &str,
    explicit: Option<&str>,
    agent: Option<rimz::agents::AgentState>,
) -> AgentsArgs {
    let root = PathBuf::from("/repo");
    let workspace = rimz::ResolvedWorkspace {
        workspace_id: WorkspaceId::from_project_root(&root),
        project_root: root,
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Repo,
        worktree_root: PathBuf::from(worktree),
        worktree_branch: None,
        session_name: "test".to_owned(),
        mux_hint: None,
    };
    let current = crate::cli::current_channel_with(&workspace, explicit.map(str::to_owned), || {
        let agent = agent?;
        let caller = rimz::harness::ancestry::CallerIdentity {
            kind: agent.kind.clone(),
            launch_id: Some(agent.agent_id.clone()),
            pane_id: None,
            name: None,
            profile: None,
            role: None,
        };
        Some((caller, vec![agent]))
    });
    let create = rimz::address::create_mention("@codex", None, current.as_deref())
        .unwrap()
        .unwrap();
    let args = create_launch_args(create, "@codex", None, None, &current, "hi");
    assert_eq!(args.launch.spec.as_deref(), Some("codex"));
    assert_eq!(args.launch.prompt.as_deref(), Some("hi"));
    args
}

#[test]
fn tier_flag_is_separate_from_the_concrete_model_flag() {
    let parsed = AgentsHarness::try_parse_from(["agents", "claude", "--tier", "senior"]);
    assert!(parsed.is_ok(), "{parsed:?}");
    assert!(
        AgentsHarness::try_parse_from(["agents", "claude", "--tier", "senior", "--model", "opus"])
            .is_err()
    );
}

fn planner_profiles() -> ProfilesConfig {
    let mut profiles = ProfilesConfig::default();
    profiles.0.insert(
        "planner".to_owned(),
        Profile {
            definition_renders: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: "claude".to_owned(),
            description: None,
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            allowed_tools: None,
            args: None,
        },
    );
    profiles
}

fn parse_agents(argv: &[&str]) -> AgentsArgs {
    AgentsHarness::try_parse_from(argv)
        .expect("parse agents command")
        .args
}

#[test]
fn prompt_guards_cover_value_taking_shorts() {
    let mut command = crate::cli::Cli::command();
    command.build();
    for (path, guarded) in [
        (vec!["agents"], AGENT_PROMPT_SHORTS),
        (vec!["agents", "launch"], AGENT_PROMPT_SHORTS),
        (vec!["teams"], COHORT_PROMPT_SHORTS),
        (vec!["teams", "launch"], COHORT_PROMPT_SHORTS),
        (vec!["subagents"], &[][..]),
        (vec!["subagents", "launch"], &[][..]),
        (vec!["message"], &[][..]),
        (vec!["pane", "send"], &[][..]),
    ] {
        let mut launch = &command;
        for name in &path {
            launch = launch.find_subcommand(name).expect("launch command");
        }
        let shorts: BTreeSet<_> = launch
            .get_arguments()
            .filter(|arg| arg.get_action().takes_values())
            .flat_map(|arg| arg.get_short_and_visible_aliases().unwrap_or_default())
            .collect();
        assert_eq!(shorts, guarded.iter().copied().collect(), "{path:?}");
    }
}

#[test]
fn launch_rejects_attached_short_prompt() {
    for prefix in [vec!["rimz", "claude"], vec!["rimz", "launch", "claude"]] {
        for (short, value) in [("-w", "feat"), ("-n", "bob")] {
            for prompt in [format!("{short}{value}"), format!("{short}={value}")] {
                let mut argv = prefix.clone();
                argv.push(&prompt);
                let error = AgentsHarness::try_parse_from(argv).expect_err("reject attached short");
                assert!(
                    error
                        .to_string()
                        .contains(&format!("write {short} NAME with a space")),
                    "{error}"
                );
            }
            let mut argv = prefix.clone();
            argv.extend([short, value]);
            let args = parse_agents(&argv);
            let launch = match args.command {
                Some(AgentsSubcmd::Launch(launch)) => *launch,
                None => args.launch,
                _ => panic!("launch"),
            };
            let actual = if short == "-w" {
                launch.cohort.worktree
            } else {
                launch.name
            };
            assert_eq!(actual.as_deref(), Some(value));
        }
    }
}

#[test]
fn compact_accepts_hyphen_instruction_and_flags() {
    for instruction in ["--keep failing test names", "-x", "-"] {
        for argv in [
            vec![
                "rimz",
                "agents",
                "compact",
                "@coder",
                "--color",
                "never",
                instruction,
            ],
            vec![
                "rimz",
                "agents",
                "compact",
                "@coder",
                instruction,
                "--color",
                "never",
            ],
        ] {
            let cli = crate::cli::Cli::try_parse_from(argv).expect("parse compact instruction");
            let Some(crate::cli::Subcmd::Agents(args)) = cli.subcommand else {
                panic!("agents")
            };
            let Some(AgentsSubcmd::Compact {
                reference,
                instruction: actual,
            }) = args.command
            else {
                panic!("compact")
            };
            assert_eq!(reference, "@coder");
            assert_eq!(actual.as_deref(), Some(instruction));
        }
    }
    for instruction in ["-h", "--help", "--color"] {
        let args = parse_agents(&["rimz", "compact", "@coder", "--", instruction]);
        let Some(AgentsSubcmd::Compact {
            instruction: actual,
            ..
        }) = args.command
        else {
            panic!("compact")
        };
        assert_eq!(actual.as_deref(), Some(instruction));
    }
}

#[test]
fn launch_accepts_hyphen_prompt_and_flags() {
    for prompt in ["--dry-run first", "-x", "-"] {
        for argv in [
            vec!["rimz", "claude", "--model", "opus", prompt],
            vec!["rimz", "claude", prompt, "--model", "opus"],
        ] {
            let args = parse_agents(&argv);
            assert_eq!(args.launch.spec.as_deref(), Some("claude"));
            assert_eq!(args.launch.prompt.as_deref(), Some(prompt));
            assert_eq!(args.launch.overrides.model.as_deref(), Some("opus"));
        }
    }
    for prompt in ["--model", "-h", "--help"] {
        let args = parse_agents(&["rimz", "claude", "--", prompt]);
        assert!(args.launch.prompt.is_none());
        assert_eq!(args.launch.overrides.passthrough, [prompt]);
    }
}

#[test]
fn profiles_parse_as_agent_profile_listings_without_legacy_aliases() {
    let args = parse_agents(&["rimz", "profiles", "--json", "--path"]);
    assert!(matches!(
        args.command,
        Some(AgentsSubcmd::Profiles {
            json: true,
            path: true,
            teams: false
        })
    ));
    let args = parse_agents(&["rimz", "profiles", "--teams"]);
    assert!(matches!(
        args.command,
        Some(AgentsSubcmd::Profiles { teams: true, .. })
    ));
    for command in ["specs", "types"] {
        let args = parse_agents(&["rimz", command]);
        assert!(args.command.is_none());
        assert_eq!(args.launch.spec.as_deref(), Some(command));
    }
}

#[test]
fn list_all_widens_to_every_lane() {
    let args = parse_agents(&["rimz", "list", "--all"]);
    assert!(matches!(
        args.command,
        Some(AgentsSubcmd::List { all: true, .. })
    ));

    // A scope and a worktree each already name one lane, so `--all` conflicts.
    for narrowing in [
        vec!["rimz", "--all", "#auth"],
        vec!["rimz", "--all", "-w", "auth"],
        vec!["rimz", "list", "--all", "#auth"],
        vec!["rimz", "list", "--all", "-w", "auth"],
    ] {
        assert!(
            AgentsHarness::try_parse_from(&narrowing).is_err(),
            "`{narrowing:?}` must refuse --all beside a lane"
        );
    }
}

#[test]
fn agent_profiles_list_only_agent_profiles_with_descriptions() {
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "planner".to_owned(),
        Profile {
            definition_renders: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: "claude".to_owned(),
            description: Some("Plans the main lane".to_owned()),
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            allowed_tools: None,
            args: None,
        },
    );
    machine.subagents.profiles.0.insert(
        "child-only".to_owned(),
        Profile {
            definition_renders: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: "codex".to_owned(),
            description: Some("Child profile".to_owned()),
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            allowed_tools: None,
            args: None,
        },
    );

    let profiles = crate::cli::profile_report::available_profiles(
        &machine.agents.profiles,
        &machine.agents.commands,
        &rimz::config::AgentSpecSources::default(),
        rimz::config::effective::ProfileScope::Agents,
    );
    assert!(!profiles.iter().any(|entry| entry.source == "kind"));
    assert!(profiles.iter().any(|entry| {
        entry.name == "planner"
            && entry.source == "profile"
            && entry.description.as_deref() == Some("Plans the main lane")
    }));
    assert!(!profiles.iter().any(|entry| entry.name == "child-only"));
}

#[test]
fn stop_when_idle_takes_an_optional_duration_or_off() {
    use super::stop::WhenIdle;
    let minutes = |minutes: u64| Some(WhenIdle::After(Duration::from_secs(minutes * 60)));
    for (argv, expected) in [
        (&["stop", "@coder"][..], None),
        (&["stop", "@coder", "--when-idle"], minutes(3)),
        (&["stop", "@coder", "--when-idle", "5m"], minutes(5)),
        (&["stop", "@coder", "--when-idle=2h"], minutes(120)),
        (&["stop", "--when-idle", "5m", "@coder"], minutes(5)),
        (
            &["stop", "@coder", "--when-idle", "0s"],
            Some(WhenIdle::After(Duration::ZERO)),
        ),
        (
            &["stop", "@coder", "--when-idle", "off"],
            Some(WhenIdle::Off),
        ),
    ] {
        let argv = [&["rimz"][..], argv].concat();
        let Some(AgentsSubcmd::Stop {
            reference,
            all,
            when_idle,
        }) = parse_agents(&argv).command
        else {
            panic!("stop")
        };
        assert_eq!((reference.as_str(), all), ("@coder", false), "{argv:?}");
        assert_eq!(when_idle, expected, "{argv:?}");
    }

    let misplaced = AgentsHarness::try_parse_from(["rimz", "stop", "--when-idle", "@me"])
        .expect_err("a reference is not a duration")
        .to_string();
    assert!(
        misplaced
            .contains("put the reference before the flag (`rimz agents stop @me --when-idle`)"),
        "{misplaced}"
    );
    for argv in [
        &["rimz", "stop", "@coder", "--when-idle", "soon"][..],
        &["rimz", "stop", "@coder", "--all", "--when-idle"],
        &["rimz", "stop", "@coder", "--when-idle", "5m", "--all"],
    ] {
        assert!(AgentsHarness::try_parse_from(argv).is_err(), "{argv:?}");
    }
}

fn parse_helper_argv(argv: Vec<String>) -> AgentsArgs {
    let argv = std::iter::once("rimz".to_owned()).chain(argv);
    let parsed = crate::cli::Cli::try_parse_from(argv).expect("parse helper argv");
    let Some(crate::cli::Subcmd::Agents(args)) = parsed.subcommand else {
        panic!("expected agents subcommand");
    };
    *args
}

#[test]
fn hidden_helper_requests_round_trip_through_cli() {
    use rimz::agents::LifecycleRefreshRequest;
    use rimz::harness::AutoContinueRequest;
    use rimz::harness::auto_redeem::{AutoRedeemRequest, RedeemReason};
    use rimz::harness::budget::BudgetParkRequest;
    use rimz::harness::idle_compact::IdleCompactRequest;
    use rimz::harness::run_timeout::RunTimeoutRequest;
    use rimz::sidebar::refresh::usage::AccountUsageRefreshRequest;

    let workspace_id = WorkspaceId::from_project_root(Path::new("/workspace with spaces"));
    let kind = AgentKind::new_unchecked("codex");
    let agent_id = AgentSessionId::from("session -- 1");
    let pane_id = PaneId::parse("tmux:%3").unwrap();

    let keepalive = rimz::harness::cache_keepalive::CacheKeepaliveRequest {
        workspace_id: workspace_id.clone(),
        kind: kind.clone(),
        agent_id: agent_id.clone(),
        pane_id: pane_id.clone(),
        anchor: "2026-01-02T03:04:05Z".parse().unwrap(),
        label: "@coder".into(),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "cache-keepalive",
        &keepalive,
    ));
    let Some(AgentsSubcmd::CacheKeepalive(args)) = parsed.command else {
        panic!("expected cache-keepalive");
    };
    assert_eq!(args.request, keepalive);

    let idle_stop = rimz::harness::idle_stop::IdleStopHelperRequest {
        workspace_id: workspace_id.clone(),
        kind: kind.clone(),
        agent_id: agent_id.clone(),
        pane_id: pane_id.clone(),
        label: "@coder # lane".into(),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "idle-stop",
        &idle_stop,
    ));
    let Some(AgentsSubcmd::IdleStop(args)) = parsed.command else {
        panic!("expected idle-stop");
    };
    assert_eq!(args.request, idle_stop);

    let auto_continue = AutoContinueRequest {
        workspace_id: workspace_id.clone(),
        kind: kind.clone(),
        agent_id: agent_id.clone(),
        pane_id: pane_id.clone(),
        message_id: Some(MessageId::parse("msg_0123456789abcdef").unwrap()),
        parked_since: "2026-01-02T03:04:05Z".parse().unwrap(),
        text: "--resume with spaces".to_owned(),
        reason: "overloaded_backoff_retry".to_owned(),
        label: Some("@coder # lane".to_owned()),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "auto-continue",
        &auto_continue,
    ));
    let Some(AgentsSubcmd::AutoContinue(args)) = parsed.command else {
        panic!("expected auto-continue");
    };
    assert_eq!(args.request, auto_continue);

    let idle_compact = IdleCompactRequest {
        workspace_id: workspace_id.clone(),
        kind: kind.clone(),
        agent_id: agent_id.clone(),
        pane_id: pane_id.clone(),
        command: "/compact --keep cache".to_owned(),
        occupied_tokens: 81_234,
        label: "@coder".to_owned(),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "idle-compact",
        &idle_compact,
    ));
    let Some(AgentsSubcmd::IdleCompact(args)) = parsed.command else {
        panic!("expected idle-compact");
    };
    assert_eq!(args.request, idle_compact);

    let auto_redeem = AutoRedeemRequest {
        workspace_id: workspace_id.clone(),
        login: rimz::ids::LoginKey::default_for(kind.clone()),
        reason: RedeemReason::ScheduledRedeem,
        request_id: "018f7f2e-7b3a-7cc0-8ec1-000000000001".parse().unwrap(),
        limit_paused: true,
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "auto-redeem",
        &auto_redeem,
    ));
    let Some(AgentsSubcmd::AutoRedeem(args)) = parsed.command else {
        panic!("expected auto-redeem");
    };
    assert_eq!(args.request, auto_redeem);

    let budget_park = BudgetParkRequest {
        workspace_id: workspace_id.clone(),
        kind: kind.clone(),
        agent_id: agent_id.clone(),
        pane_id: pane_id.clone(),
        at_cost: Some(12.75),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "budget-park",
        &budget_park,
    ));
    let Some(AgentsSubcmd::BudgetPark(args)) = parsed.command else {
        panic!("expected budget-park");
    };
    assert_eq!(args.request, budget_park);

    let run_timeout = RunTimeoutRequest {
        workspace_id: workspace_id.clone(),
        run_id: RunId::parse("run_0123456789abcdef0123456789abcdef").unwrap(),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "run-timeout",
        &run_timeout,
    ));
    let Some(AgentsSubcmd::RunTimeout(args)) = parsed.command else {
        panic!("expected run-timeout");
    };
    assert_eq!(args.request, run_timeout);

    let orphan_subagent = OrphanSubagentRequest {
        workspace_id: workspace_id.clone(),
        child_kind: kind.clone(),
        child_agent_id: agent_id.clone(),
        parent_agent_id: AgentSessionId::from("parent -- 1"),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "orphan-subagent",
        &orphan_subagent,
    ));
    let Some(AgentsSubcmd::OrphanSubagent(args)) = parsed.command else {
        panic!("expected orphan-subagent");
    };
    assert_eq!(args.request, orphan_subagent);

    let refresh_usage = AccountUsageRefreshRequest {
        workspace_id: workspace_id.clone(),
        login: rimz::ids::LoginKey::default_for(kind.clone()),
        claim_id: "018f7f2e-7b3a-7cc0-8ec1-000000000002".parse().unwrap(),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "refresh-usage",
        &refresh_usage,
    ));
    let Some(AgentsSubcmd::RefreshUsage(args)) = parsed.command else {
        panic!("expected refresh-usage");
    };
    assert_eq!(args.request, refresh_usage);

    let refresh_context = LifecycleRefreshRequest {
        kind,
        session_id: "session -- 1".to_owned(),
        workspace_id,
        model: Some("gpt 5 -- fast".to_owned()),
        server_url: Some("http://127.0.0.1:4096/path with spaces".to_owned()),
    };
    let parsed = parse_helper_argv(rimz::child_process::agent_helper_argv(
        "refresh-context",
        &refresh_context,
    ));
    assert_eq!(
        parsed.scope(),
        (
            "agents refresh-context",
            Some(refresh_context.session_id.as_str()),
            Some(refresh_context.kind.as_str()),
        )
    );
    let Some(AgentsSubcmd::RefreshContext(args)) = parsed.command else {
        panic!("expected refresh-context");
    };
    assert_eq!(args.request, refresh_context);
}

fn assert_clap_error(argv: &[&str], kind: clap::error::ErrorKind) {
    let err = AgentsHarness::try_parse_from(argv).expect_err("invalid form");
    assert_eq!(err.kind(), kind, "{argv:?}");
}

#[test]
fn reserved_agent_words_name_current_verbs() {
    let command = AgentsHarness::command();
    let mut verbs = BTreeSet::new();
    for subcommand in command.get_subcommands() {
        verbs.insert(subcommand.get_name().to_owned());
        verbs.extend(subcommand.get_all_aliases().map(str::to_owned));
    }
    for word in rimz::agents::petname::RESERVED_AGENT_WORDS {
        if *word == "term" {
            continue;
        }
        assert!(
            verbs.contains(*word),
            "reserved word `{word}` is not an agents verb"
        );
    }
}

mod parse {
    use super::*;

    #[test]
    fn global_root_override_remains_and_top_level_flag_is_removed() {
        let parsed =
            crate::cli::Cli::try_parse_from(["rimz", "agents", "--root", "/repo", "claude"])
                .expect("global root override");
        assert_eq!(parsed.global.root.as_deref(), Some(Path::new("/repo")));
        assert!(
            crate::cli::Cli::try_parse_from(["rimz", "agents", "claude", "task", "--top-level"])
                .is_err()
        );
    }

    #[test]
    fn launch_forms_parse_public_contract() {
        let _ = parse_agents(&["rimz", "claude", "task", "--cwd", "/tmp/clean"]);
        let (request, _) = into_supervised_request(parse_agents(&[
            "rimz",
            "claude",
            "task",
            "-p",
            "--cwd",
            "/tmp/clean",
        ]))
        .unwrap();
        assert_eq!(request.cwd.as_deref(), Some(Path::new("/tmp/clean")));
        for flag in ["--worktree=demo", "--from-pr=12", "--resume", "--fresh"] {
            let error =
                AgentsHarness::try_parse_from(["rimz", "claude", "--cwd", "/tmp/clean", flag])
                    .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
        let argv: Vec<_> = "rimz claude,codex+term fix-tests --worktree=docs --bg"
            .split_ascii_whitespace()
            .collect();
        let args = parse_agents(&argv);
        assert_eq!(
            [
                args.launch.spec.as_deref(),
                args.launch.prompt.as_deref(),
                args.launch.cohort.worktree.as_deref()
            ],
            [Some("claude,codex+term"), Some("fix-tests"), Some("docs")]
        );
        assert!(args.launch.cohort.bg);

        let argv: Vec<_> = "rimz claude fix-auth --from-pr https://gitlab.com/org/repo/-/merge_requests/12 --worktree review-12 --model opus --description port-auth --effort high --system-prompt-file /abs/prompt.md -p --max-turns 3 --retries 2 --verify true --max-attempts 4 -n swift-otter"
            .split_ascii_whitespace()
            .collect();
        let args = parse_agents(&argv);
        assert_eq!(
            (
                args.launch.spec.as_deref(),
                args.launch.prompt.as_deref(),
                args.launch.cohort.worktree.as_deref(),
                args.launch.overrides.model.as_deref(),
                args.launch.cohort.description.as_deref(),
                args.launch.overrides.effort.as_deref(),
                args.launch.overrides.system_prompt_file.as_deref(),
                args.launch.max_turns,
            ),
            (
                Some("claude"),
                Some("fix-auth"),
                Some("review-12"),
                Some("opus"),
                Some("port-auth"),
                Some("high"),
                Some(Path::new("/abs/prompt.md")),
                Some(3),
            )
        );
        assert_eq!(
            (
                args.launch.retries,
                args.launch.verify.as_deref(),
                args.launch.max_attempts,
                args.launch.name.as_deref(),
            ),
            (Some(2), Some("true"), Some(4), Some("swift-otter"),)
        );
        assert_eq!(
            args.launch.cohort.from_pr.unwrap().forge,
            Some(Forge::GitLab)
        );

        let resume = parse_agents(&["rimz", "forge", "--resume"]);
        let alias = parse_agents(&["rimz", "forge", "--continue"]);
        let scoped = parse_agents(&[
            "rimz",
            "forge",
            "--worktree=restore-living-team",
            "--resume",
        ]);
        assert!(
            resume.launch.cohort.resume
                && alias.launch.cohort.resume
                && scoped.launch.cohort.resume
        );
        assert_eq!(
            scoped.launch.cohort.worktree.as_deref(),
            Some("restore-living-team")
        );
    }

    #[test]
    fn launch_verb_and_bare_form_parse_the_same_payload() {
        let bare = parse_agents(&["rimz", "claude", "ship", "-p"]);
        let verb = parse_agents(&["rimz", "launch", "claude", "ship", "-p"]);
        let Some(AgentsSubcmd::Launch(verb)) = verb.command else {
            panic!("launch verb");
        };

        assert_eq!(bare.launch, *verb);
    }

    #[test]
    fn launch_overrides_conflict_with_resume() {
        for override_args in [
            vec!["--ask"],
            vec!["--yolo"],
            vec!["--model", "opus"],
            vec!["--effort", "high"],
            vec!["--isolation", "host"],
            vec!["--agent", "claude"],
            vec!["--tier", "senior"],
            vec!["ship"],
        ] {
            for prefix in [vec!["rimz", "claude"], vec!["rimz", "launch", "claude"]] {
                for resume in ["--resume", "--continue"] {
                    let mut argv = prefix.clone();
                    argv.push(resume);
                    argv.extend_from_slice(&override_args);
                    let args = parse_agents(&argv);
                    let launch = match &args.command {
                        Some(AgentsSubcmd::Launch(launch)) => launch.as_ref(),
                        _ => &args.launch,
                    };
                    assert!(launch.cohort.resume);
                    validate_resume_inputs(launch, ResumeEntrance::Flag).unwrap();
                    validate_resume_inputs(launch, ResumeEntrance::Reconcile).unwrap();
                }
            }
        }
        for (input, override_args, message) in [
            (
                "--system-prompt-file",
                vec!["--system-prompt-file", "base.md"],
                "resume takes system-prompt files from the current profile; update the profile or launch fresh instead of passing `--system-prompt-file`",
            ),
            (
                "--append-system-prompt-file",
                vec!["--append-system-prompt-file", "fragment.md"],
                "resume takes system-prompt files from the current profile; update the profile or launch fresh instead of passing `--append-system-prompt-file`",
            ),
            (
                "passthrough arguments",
                vec!["--", "--foo"],
                "resume does not accept passthrough arguments after `--`; put supported settings in the profile or launch fresh",
            ),
        ] {
            for resume in [Some("--resume"), Some("--continue"), None] {
                let mut argv = vec!["rimz", "claude"];
                argv.extend(resume);
                argv.extend_from_slice(&override_args);
                let args = parse_agents(&argv);
                let (entrance, expected) = if resume.is_some() {
                    (ResumeEntrance::Flag, message.to_owned())
                } else {
                    (
                        ResumeEntrance::Reconcile,
                        format!(
                            "{message}; rerun without {input}, or choose fresh instead of resume"
                        ),
                    )
                };
                let error = validate_resume_inputs(&args.launch, entrance)
                    .expect_err("resume refuses this input");
                assert_eq!(error.to_string(), expected);
            }
        }
        assert_clap_error(
            &["rimz", "claude", "--ask", "--yolo"],
            clap::error::ErrorKind::ArgumentConflict,
        );
        assert_clap_error(
            &["rimz", "claude", "--resume", "--ask", "--yolo"],
            clap::error::ErrorKind::ArgumentConflict,
        );
    }

    #[test]
    fn fresh_launch_parses_and_conflicts_with_resume_and_from_pr() {
        for argv in [
            vec!["rimz", "forge", "--fresh"],
            vec!["rimz", "forge", "--worktree=topic", "--fresh"],
        ] {
            assert!(parse_agents(&argv).launch.cohort.fresh);
        }
        for conflicting in ["--resume", "--continue"] {
            assert_clap_error(
                &["rimz", "forge", "-w", "topic", "--fresh", conflicting],
                clap::error::ErrorKind::ArgumentConflict,
            );
        }
        assert_clap_error(
            &["rimz", "forge", "--fresh", "--from-pr", "1"],
            clap::error::ErrorKind::ArgumentConflict,
        );
        let supervised = parse_agents(&["rimz", "claude", "ship", "-w", "topic", "--fresh", "-p"]);
        assert!(
            into_supervised_request(supervised)
                .err()
                .expect("fresh is interactive-only")
                .to_string()
                .contains("not -p")
        );
    }

    #[test]
    fn lane_resume_forms_parse_public_contract() {
        let scoped = parse_agents(&["rimz", "resume", "#docs"]);
        let pr = parse_agents(&[
            "rimz",
            "resume",
            "--from-pr",
            "https://github.com/rimz/rimz/pull/69",
            "--bg",
        ]);
        assert!(matches!(
            scoped.command,
            Some(AgentsSubcmd::Resume {
                scope: Some(scope),
                worktree: None,
                from_pr: None,
                bg: false,
                fresh: false,
            })
                if scope == "#docs"
        ));
        assert!(matches!(
            pr.command,
            Some(AgentsSubcmd::Resume {
                scope: None,
                worktree: None,
                from_pr: Some(rimz::forge::PrTarget { number: 69, .. }),
                bg: true,
                fresh: false,
            })
        ));
        assert_clap_error(
            &["rimz", "resume", "#docs", "--from-pr", "69"],
            clap::error::ErrorKind::ArgumentConflict,
        );
    }

    #[test]
    fn lane_resume_fresh_preserves_scope_and_background_options() {
        for argv in [
            vec!["rimz", "resume", "#docs", "--fresh", "--bg"],
            vec!["rimz", "resume", "-w", "docs", "--fresh", "--bg"],
            vec!["rimz", "resume", "--from-pr", "69", "--fresh", "--bg"],
        ] {
            let args = parse_agents(&argv);
            assert!(matches!(
                args.command,
                Some(AgentsSubcmd::Resume {
                    bg: true,
                    fresh: true,
                    ..
                })
            ));
        }
    }

    #[test]
    fn invalid_launch_forms_report_clap_contracts() {
        use clap::error::ErrorKind::{ArgumentConflict, MissingRequiredArgument};

        for (argv, kind) in [
            (&["rimz", "launch"][..], MissingRequiredArgument),
            (&["rimz", "list", "#docs", "--all"][..], ArgumentConflict),
            (&["rimz", "attribution", "--json", "--md"], ArgumentConflict),
            (
                &["rimz", "show", "swift-otter", "--ansi"],
                MissingRequiredArgument,
            ),
            (&["rimz", "refresh", "@codex", "--all"], ArgumentConflict),
            (
                &["rimz", "claude", "hi", "--output-format", "json"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "claude", "hi", "--max-turns", "3"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "claude", "hi", "--retries", "1"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "claude", "hi", "-p", "--retries", "1", "--bg"],
                ArgumentConflict,
            ),
            (
                &["rimz", "claude", "hi", "-p", "--verify", "true", "--bg"],
                ArgumentConflict,
            ),
            (
                &["rimz", "claude", "hi", "-p", "--max-attempts", "2"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "wait", "codex", "--from-start"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "wait", "otter", "--any", "--stream"],
                ArgumentConflict,
            ),
            (&["rimz", "wait"], MissingRequiredArgument),
            (
                &["rimz", "claude", "--new-pane", "--new-tab"],
                ArgumentConflict,
            ),
        ] {
            assert_clap_error(argv, kind);
        }
        for override_args in [
            &["--channel=design"][..],
            &["--from-pr", "1"],
            &["--name", "swift-otter"],
            &["--description", "work"],
            &["--budget", "5"],
            &["-p"],
        ] {
            let argv = [vec!["rimz", "claude", "--resume"], override_args.to_vec()].concat();
            assert_clap_error(&argv, ArgumentConflict);
        }
    }

    #[test]
    fn invalid_supervised_output_combinations_fail_fast() {
        for (argv, output, fragment) in [
            (
                &["rimz", "claude", "hi", "-p", "--bg"][..],
                OutputFormat::StreamJson,
                "cannot be combined with --bg",
            ),
            (
                &["rimz", "claude", "hi", "-p", "--retries", "1"],
                OutputFormat::StreamJson,
                "choose text or json",
            ),
            (
                &["rimz", "claude", "hi", "-p", "--verify", "true"],
                OutputFormat::StreamJson,
                "choose text or json",
            ),
            (
                &[
                    "rimz",
                    "claude",
                    "hi",
                    "-p",
                    "--verify",
                    "true",
                    "--max-attempts",
                    "0",
                ],
                OutputFormat::Text,
                "at least 1",
            ),
        ] {
            let args = parse_agents(argv);
            let err = validate_supervised_output(&args, output).expect_err("reject output");
            assert!(err.to_string().contains(fragment), "{argv:?}: {err:#}");
        }
    }

    #[test]
    fn detach_is_refused_where_no_launch_report_exists_to_drop() {
        for (argv, error) in [
            (
                &["rimz", "claude", "hi", "-p", "--detach"][..],
                "--detach on `-p` requires --bg: a foreground run prints its answer inline; add --bg, or drop --detach",
            ),
            (
                &["rimz", "claude", "--resume", "--detach"],
                "--detach cannot be combined with --resume: a resumed cohort has no launch task to detach; drop --detach",
            ),
            (
                &["rimz", "claude", "--detach"],
                "--detach requires a prompt: without a launch task no report is owed; add a prompt, or drop --detach",
            ),
            (
                &["rimz", "claude", " \t", "--detach"],
                "--detach requires a prompt: without a launch task no report is owed; add a prompt, or drop --detach",
            ),
        ] {
            let args = parse_agents(argv);
            let err = validate_detach(&args.launch, args.launch.print).expect_err("refuse detach");
            assert_eq!(err.to_string(), error, "{argv:?}");
        }
        for argv in [
            &["rimz", "claude", "hi", "--detach"][..],
            &["rimz", "claude", "hi", "-p", "--bg", "--detach"],
            &["rimz", "claude", "--resume"],
            &["rimz", "claude", "-p"],
        ] {
            let args = parse_agents(argv);
            validate_detach(&args.launch, args.launch.print)
                .unwrap_or_else(|err| panic!("{argv:?}: {err:#}"));
        }
        let loop_check = parse_agents(&["rimz", "claude", "hi", "--detach"]);
        assert!(
            validate_detach(&loop_check.launch, true).is_err(),
            "a loop check is a foreground print run whatever its flags say"
        );
    }

    #[test]
    fn no_verb_without_a_launch_spec_refuses_with_the_list_command() {
        for (argv, list) in [
            (&["rimz"][..], "rimz agents list"),
            (
                &["rimz", "--all", "--json"],
                "rimz agents list --all --json",
            ),
            (&["rimz", "-w", "feat"], "rimz agents list --worktree feat"),
            (
                &["rimz", "-w", "feat", "--json"],
                "rimz agents list --worktree feat --json",
            ),
            (&["rimz", "-w"], "rimz agents list"),
            (&["rimz", ""], "rimz agents list"),
            (&["rimz", " \t", "--json"], "rimz agents list --json"),
            (&["rimz", "--yolo"], "rimz agents list"),
            (&["rimz", "--from-pr", "1"], "rimz agents list"),
            (&["rimz", "--channel", "docs"], "rimz agents list"),
            (&["rimz", "--", "term"], "rimz agents list"),
            (&["rimz", "--model", "opus"], "rimz agents list"),
            (&["rimz", "--tier", "senior"], "rimz agents list"),
            (&["rimz", "--fresh"], "rimz agents list"),
            (&["rimz", "--isolation", "sandbox"], "rimz agents list"),
            (&["rimz", "-p", "--max-turns", "3"], "rimz agents list"),
            (&["rimz", "--detach"], "rimz agents list"),
        ] {
            let args = parse_agents(argv);
            let refusal = launch_refusal(&args.launch, args.all, args.json)
                .unwrap_or_else(|| panic!("{argv:?} must refuse"))
                .to_string();
            let first = refusal.lines().next().expect("fix line");
            assert_eq!(
                first,
                format!(
                    "`rimz agents` needs a verb or a launch spec; to list agents, run `{list}`"
                ),
                "{argv:?}"
            );
        }
        for (argv, list) in [
            (&["rimz", "#auth"][..], "rimz agents list '#auth'"),
            (
                &["rimz", "#auth", "hi", "--json"],
                "rimz agents list '#auth' --json",
            ),
            (&["rimz", "#auth", "--yolo"], "rimz agents list '#auth'"),
        ] {
            let args = parse_agents(argv);
            let refusal = launch_refusal(&args.launch, args.all, args.json)
                .unwrap_or_else(|| panic!("{argv:?} must refuse"))
                .to_string();
            assert_eq!(
                refusal,
                format!("`#auth` is a scope, not a launch spec; to list that lane, run `{list}`"),
                "{argv:?}"
            );
        }
        for argv in [
            &["rimz", "claude"][..],
            &["rimz", "claude", "hi", "--yolo"],
            &["rimz", "forge.planner"],
            &["rimz", "@coder"],
        ] {
            let args = parse_agents(argv);
            assert!(
                launch_refusal(&args.launch, args.all, args.json).is_none(),
                "{argv:?} names a launch target"
            );
        }
    }
}

#[test]
fn refresh_targets_honor_channel_filter() {
    let now = Timestamp::from_second(2_000).unwrap();
    let pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut auth = rimz::testkit::agent_state("claude", "auth", now);
    auth.channel = Some("auth-refresh".to_owned());
    auth.worktree_path = Some("/repo/worktrees/auth-refresh".to_owned());
    auth.pane = Some(rimz::pane::PaneRef::from_id(pane_id.clone()));
    let mut auth_shadow = rimz::testkit::agent_state("claude", "auth-shadow", now);
    auth_shadow.channel = auth.channel.clone();
    auth_shadow.worktree_path = auth.worktree_path.clone();
    auth_shadow.pane = auth.pane.clone();
    let mut docs = rimz::testkit::agent_state("codex", "docs", now);
    docs.channel = Some("docs".to_owned());
    docs.worktree_path = Some("/repo/main".to_owned());
    let mut child = rimz::testkit::agent_state("claude", "child", now);
    child.channel = auth.channel.clone();
    child.parent_agent_id = Some(AgentSessionId::from("auth"));
    let mut unknown = rimz::testkit::agent_state("ghost", "unknown", now);
    unknown.channel = auth.channel.clone();
    let mut snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/repo/main")),
        vec![auth, auth_shadow, docs, child, unknown],
        now,
    );
    let owner = &snapshot.agents[0];
    let owner_pane = rimz::store::snapshot::PaneAgent {
        root_lane: false,
        kind: owner.kind.clone(),
        kind_ordinal: owner.kind_ordinal,
        name: owner.name.clone(),
        name_explicit: owner.name_explicit,
        profile: owner.profile.clone(),
        role: owner.role.clone(),
        channel: owner.channel.clone(),
        agent_id: Some(owner.agent_id.clone()),
        pane_id,
        pane_pid: None,
        worktree_path: owner.worktree_path.clone(),
        worktree_branch: owner.worktree_branch.clone(),
    };
    snapshot.agent_panes = vec![owner_pane];

    let scoped: Vec<&str> = super::refresh::refresh_targets(&snapshot, Some("auth-refresh"))
        .into_iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(scoped, vec!["auth"]);

    let workspace: Vec<&str> = super::refresh::refresh_targets(&snapshot, None)
        .into_iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(workspace, vec!["auth", "docs"]);
}

mod launch_options {
    use super::*;

    #[test]
    fn tier_override_changes_the_interactive_launch_runtime() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        for kind in ["claude", "codex"] {
            std::fs::write(
                root.path().join(format!("agents/{kind}.md")),
                "---\ndescription: Base\n---\nBase.",
            )
            .unwrap();
        }
        std::fs::write(
            root.path().join("agents/worker.md"),
            "---\ndescription: Worker\nagent: codex\ntier: senior\ntools: [Bash]\n---\nCraft.",
        )
        .unwrap();
        let mut machine = MachineConfig::default();
        let definitions = rimz::config::definitions::load(
            root.path(),
            rimz::config::definitions::SkillCheck::Skip,
            &machine.agents.commands,
            &machine.tiers,
        );
        assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
        machine.agents.profiles = definitions.agent_profiles;
        let args = AgentsHarness::try_parse_from([
            "agents",
            "worker",
            "--tier",
            "principal",
            "--agent",
            "codex",
        ])
        .unwrap()
        .args;
        let (resolved, _) = resolve_and_validate(&args, &machine, root.path()).unwrap();
        let cell = resolved.layout.agent_cells().next().unwrap();
        assert_eq!(cell.kind.as_str(), "claude");
        assert_eq!(cell.launch.model.as_deref(), Some("fable"));
        assert!(cell.args.iter().any(|arg| arg == "--tools"));
    }

    #[test]
    fn loop_check_launch_is_blocking_bounded_and_owned() {
        let mut config = MachineConfig::default();
        config.r#loop.default_timeout = Some("17m".to_owned());
        for (flags, keep, seconds) in [
            (vec![], false, 17 * 60),
            (vec!["-p", "--keep", "--timeout", "30s"], true, 30),
        ] {
            let mut argv = vec!["rimz", "claude", "check"];
            argv.extend(flags);
            let (request, _) =
                into_loop_check_request(parse_agents(&argv), "nightly", &config).unwrap();
            assert_eq!(request.loop_task.as_deref(), Some("nightly"));
            // The check script wrote this prompt, not the rule.
            assert_eq!(request.loop_reminder, None);
            assert!(!request.background);
            assert_eq!(request.keep, keep);
            assert_eq!(request.self_cleanup_on_completion, !keep);
            assert_eq!(
                request.timeout,
                Some(std::time::Duration::from_secs(seconds))
            );
            assert!(request.force_new_tab);
        }
        // `--new-tab` stays accepted and changes nothing.
        let (request, _) = into_loop_check_request(
            parse_agents(&["rimz", "claude", "check", "--new-tab"]),
            "nightly",
            &config,
        )
        .unwrap();
        assert!(request.force_new_tab);
        for (argv, expected) in [
            (vec!["rimz", "claude"], "with a prompt"),
            (vec!["rimz", "claude", "check", "--bg"], "remove `--bg`"),
        ] {
            let error = into_loop_check_request(parse_agents(&argv), "nightly", &config)
                .err()
                .expect("refused loop launch");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn supervised_request_carries_profile_base_model_and_effort_overrides() {
        let args = parse_agents(&[
            "rimz",
            "codex",
            "fix-it",
            "--agent",
            " reviewer ",
            "--model",
            " gpt-5 ",
            "--effort",
            " low ",
            "-p",
        ]);

        let (request, _) = into_supervised_request(args).expect("build supervised request");

        assert_eq!(request.agent.as_deref(), Some("reviewer"));
        assert_eq!(request.model.as_deref(), Some(" gpt-5 "));
        assert_eq!(request.effort.as_deref(), Some(" low "));
    }

    #[test]
    fn supervised_request_preserves_internal_launch_posture() {
        for (background, caller_owned, subagent, expected_cleanup) in [
            (false, false, false, false),
            (true, false, false, true),
            (false, true, true, true),
        ] {
            let args = AgentsArgs::from_launch(AgentLaunchArgs {
                spec: Some("codex".to_owned()),
                prompt: Some("fix-it".to_owned()),
                cohort: CohortLaunchArgs {
                    bg: background,
                    ..Default::default()
                },
                print: true,
                self_cleanup_on_completion: caller_owned,
                subagent,
                ..Default::default()
            });

            let (request, _) = into_supervised_request(args).expect("build supervised request");

            assert_eq!(
                request.self_cleanup_on_completion, expected_cleanup,
                "background={background}, caller_owned={caller_owned}"
            );
            assert_eq!(request.subagent, subagent);
        }
    }

    #[test]
    fn agent_override_parses_and_unknown_value_lists_choices() {
        let args = parse_agents(&["rimz", "coder", "--agent", "claude"]);
        assert_eq!(args.launch.overrides.agent.as_deref(), Some("claude"));
        assert_eq!(
            rimz::harness::plan::normalized_preset_value(args.launch.overrides.agent.as_deref())
                .as_deref(),
            Some("claude")
        );

        let dir = tempfile::tempdir().expect("temp dir");
        let machine = MachineConfig::default();
        let effective = rimz::config::effective::load_with_roots(
            &machine,
            dir.path(),
            &dir.path().join("config"),
        )
        .expect("effective config");
        let err = rimz::harness::plan::resolve_launch(
            &effective,
            rimz::config::effective::ProfileScope::Agents,
            &machine.agents.commands,
            Some("codex"),
            Some("ghost"),
        )
        .expect_err("unknown");
        let message = err.to_string();
        assert!(
            message.contains("unknown agent profile or kind `ghost`"),
            "{message}"
        );
        assert!(message.contains("claude"), "{message}");
        assert!(message.contains("codex"), "{message}");
    }

    fn resolve_and_validate(
        args: &AgentsArgs,
        machine: &MachineConfig,
        root: &Path,
    ) -> Result<(ResolvedLaunch, LaunchPreset)> {
        let effective =
            rimz::config::effective::load_with_roots(machine, root, &root.join("config-home"))?;
        let finalized = resolve_finalized_layout(
            None,
            machine,
            &effective,
            args.launch.spec.as_deref(),
            args.launch.prompt.as_deref(),
            &args.launch.overrides,
            args.launch.cohort.budget,
            args.launch.max_turns,
            None,
            args.launch.name.is_some(),
            None,
        )?;
        Ok((finalized.resolved, finalized.preset))
    }

    #[test]
    fn finalized_layout_resolves_without_store_and_qualifies_only_the_selected_lane() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut machine = MachineConfig::default();
        machine.agents.profiles = planner_profiles();
        machine.agents.teams.0.insert(
            "forge".to_owned(),
            rimz::config::Team {
                roles: vec![rimz::config::RoleBinding {
                    role: "planner".to_owned(),
                    profile: "planner".to_owned(),
                    signals: Vec::new(),
                    owns: Vec::new(),
                    flip_compact: None,
                    idle_compact: None,
                    keep_warm: None,
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    auto_compact: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                }],
                ..Default::default()
            },
        );
        let effective = rimz::config::effective::load_with_roots(
            &machine,
            dir.path(),
            &dir.path().join("config-home"),
        )
        .expect("effective config");
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(dir.path()),
            vec![agent_in_lane("planner", Some("topic"), None, Some("forge"))],
            Timestamp::from_second(1_000).unwrap(),
        );
        let overrides = LaunchOverrideArgs {
            yolo: true,
            model: Some("chosen".to_owned()),
            ..Default::default()
        };
        for (snapshot, lane, inferred) in [
            (None, Some("topic"), None),
            (Some(&snapshot), None, None),
            (Some(&snapshot), Some("other"), None),
            (Some(&snapshot), Some("topic"), Some("topic")),
        ] {
            let finalized = resolve_finalized_layout(
                snapshot,
                &machine,
                &effective,
                Some("planner"),
                None,
                &overrides,
                Some("5".parse().expect("budget")),
                None,
                lane,
                false,
                None,
            )
            .expect("finalized layout");
            let cell = finalized.resolved.layout.agent_cells().next().unwrap();
            assert_eq!(cell.kind.as_str(), "claude");
            assert_eq!(cell.launch.role.as_deref(), inferred.map(|_| "planner"));
            assert_eq!(cell.launch.mode, Some(PermissionMode::Yolo));
            assert_eq!(cell.launch.model.as_deref(), Some("chosen"));
            assert_eq!(cell.launch.budget.as_deref(), Some("$5.00"));
            assert_eq!(finalized.inferred_lane.as_deref(), inferred);
            assert_eq!(
                finalized.qualified_spec.as_deref(),
                inferred.map(|_| "forge.planner")
            );
        }
    }

    #[test]
    fn prompt_file_flags_resolve_and_reject_bad_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "be concise").expect("write prompt");
        let system_flag = format!("--system-prompt-file={}", prompt.display());
        let args = parse_agents(&["rimz", "claude", "hi", &system_flag]);
        let preset = launch_override_preset(&args.launch.overrides).expect("resolve prompt files");
        assert_eq!(
            preset.system_prompt_file,
            Some(prompt.canonicalize().unwrap())
        );
        let fragment = dir.path().join("fragment.md");
        std::fs::write(&fragment, "shared rules").expect("write fragment");
        let fragment_path = fragment.to_str().expect("utf8 fragment");
        let args = parse_agents(&[
            "rimz",
            "claude",
            "hi",
            "--append-system-prompt-file",
            fragment_path,
            "--append-system-prompt-file",
            fragment_path,
        ]);
        let preset = launch_override_preset(&args.launch.overrides).expect("resolve fragments");
        assert_eq!(
            preset.append_system_prompt_files,
            [
                fragment.canonicalize().unwrap(),
                fragment.canonicalize().unwrap()
            ]
        );

        let dir_path = dir.path().to_str().expect("utf8 dir path");
        let args = parse_agents(&["rimz", "claude", "hi", "--system-prompt-file", dir_path]);
        let err = launch_override_preset(&args.launch.overrides).expect_err("reject a directory");
        assert!(err.to_string().contains("is not a regular file"), "{err:#}");

        let missing = dir.path().join("missing.md");
        let missing_path = missing.to_str().expect("utf8 missing path");
        let missing_flag = format!("--system-prompt-file={missing_path}");
        let args = parse_agents(&["rimz", "claude", "hi", &missing_flag]);
        let err =
            launch_override_preset(&args.launch.overrides).expect_err("reject missing prompt path");
        assert!(
            err.to_string().contains("reading --system-prompt-file"),
            "{err:#}"
        );
    }

    #[test]
    fn unknown_spec_errors_precede_secondary_launch_validation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("missing.md");
        let missing_path = missing.to_str().expect("utf8 path");
        let ambiguous = ["rimz", "missing-agent", "claude"];
        let missing_flag = format!("--system-prompt-file={missing_path}");
        let missing_file = ["rimz", "missing-agent", &missing_flag];
        for (argv, secondary_error) in [
            (&ambiguous[..], "looks like another spec"),
            (&missing_file[..], "system-prompt-file"),
        ] {
            let args = parse_agents(argv);
            let err = resolve_and_validate(&args, &MachineConfig::default(), dir.path())
                .expect_err("unknown spec wins");
            let message = err.to_string();
            assert!(message.contains("missing-agent"), "{err:#}");
            assert!(!message.contains(secondary_error), "{err:#}");
        }
    }

    #[test]
    fn spec_like_prompt_fails_before_name_and_prompt_file_validation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let args = parse_agents(&[
            "rimz",
            "claude,codex",
            "claude",
            "--name",
            "one",
            "--system-prompt-file",
            "missing.md",
        ]);
        let err = resolve_and_validate(&args, &MachineConfig::default(), dir.path())
            .expect_err("spec-like prompt wins");
        assert!(
            err.to_string().contains("looks like another spec cell"),
            "{err:#}"
        );
    }

    #[test]
    fn multi_cell_name_fails_before_finalize_warnings() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut machine = MachineConfig::default();
        machine.agents.profiles.0.insert(
            "warn".to_owned(),
            rimz::config::Profile {
                definition_renders: None,
                model_tier: None,
                tier_stamp: None,
                isolation: None,
                auto_compact: None,
                agent: "codex".to_owned(),
                description: None,
                subagents: None,
                model_reminder: None,
                keep_warm: None,
                mode: None,
                model: Some("declared".to_owned()),
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                skills: None,
                allowed_tools: None,
                args: Some("--model raw".to_owned()),
            },
        );
        let args = parse_agents(&["rimz", "warn,codex", "--name", "one"]);
        let effective = rimz::config::effective::load_with_roots(
            &machine,
            dir.path(),
            &dir.path().join("config-home"),
        )
        .expect("effective config");
        let resolved = rimz::harness::plan::resolve_launch(
            &effective,
            rimz::config::effective::ProfileScope::Agents,
            &machine.agents.commands,
            args.launch.spec.as_deref(),
            None,
        )
        .expect("resolve warning-capable layout");

        let err = resolve_finalized_layout(
            None,
            &machine,
            &effective,
            args.launch.spec.as_deref(),
            args.launch.prompt.as_deref(),
            &args.launch.overrides,
            args.launch.cohort.budget,
            args.launch.max_turns,
            None,
            args.launch.name.is_some(),
            None,
        )
        .expect_err("name cardinality wins");
        assert_eq!(
            err.to_string(),
            "--name requires a layout with exactly one agent cell"
        );

        let mut warning_layout = resolved.layout;
        let warnings = rimz::harness::plan::finalize_launch_layout(
            &mut warning_layout,
            LaunchFinalizeOptions {
                agent_base: None,
                permission_mode: None,
                isolation: None,
                preset: &LaunchPreset::default(),
                passthrough: &[],
                budget: None,
                max_turns: None,
            },
        )
        .expect("layout can finalize");
        assert!(!warnings.is_empty(), "fixture must be warning-capable");
    }
}

mod render {
    use super::*;

    #[test]
    fn agents_table_shows_petnames_profiles_and_kinds() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut first =
            agent_with_status("first", AgentStatus::Running, TurnPhase::Reasoning, 1_000);
        first.name = Some("calm-fox".to_owned());
        first.profile = Some("planner".to_owned());
        first.kind_ordinal = Some(1);
        let mut second = first.clone();
        second.agent_id = "second".into();
        second.name = Some("bright-lark".to_owned());
        second.kind_ordinal = Some(2);
        second.login = Some("work".parse().unwrap());
        let mut bare = first.clone();
        bare.agent_id = "bare".into();
        bare.name = Some("swift-otter".to_owned());
        bare.profile = None;
        bare.kind_ordinal = Some(3);
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![first, second, bare],
            now,
        );
        let text = render_agents_text(&snapshot, now, 180);
        assert_eq!(
            text.lines()
                .next()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            [
                "HANDLE", "PROFILE", "AGENT", "STATUS", "MODEL", "CTX", "TOKENS", "AGE"
            ],
            "{text}"
        );
        for (handle, profile, kind) in [
            ("@calm-fox", "planner", "claude"),
            ("@bright-lark", "planner", "claude@work"),
            ("@swift-otter", "-", "claude"),
        ] {
            let row = text
                .lines()
                .find(|line| line.contains(handle))
                .expect("petname row");
            assert_eq!(
                row.split_whitespace().take(3).collect::<Vec<_>>(),
                [handle, profile, kind],
                "{text}"
            );
        }
    }

    #[test]
    fn agents_table_projects_public_row_contract() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut failed = agent_with_turn_error(
            agent_with_status(
                "failed-sess",
                AgentStatus::Running,
                TurnPhase::Reasoning,
                1_000,
            ),
            TurnErrorClass::Failed,
            1_010,
            "API Error: Bad Request",
        );
        failed.name = Some("writer".to_owned());
        failed.name_explicit = true;
        failed.description = Some("fix failing auth flow".to_owned());
        let paused = agent_with_turn_error(
            agent_with_status(
                "paused-sess",
                AgentStatus::Running,
                TurnPhase::Reasoning,
                1_000,
            ),
            TurnErrorClass::PausedOverloaded,
            1_010,
            "API Error: Overloaded",
        );
        let running = agent_with_status(
            "running-sess",
            AgentStatus::Running,
            TurnPhase::Reasoning,
            1_000,
        );
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![failed, paused, running],
            now,
        );
        let text = render_agents_text(&snapshot, now, 120);

        let header = text.lines().next().unwrap_or_default();
        assert!(
            text.contains("@writer")
                && !text.contains("@claude")
                && !header.contains("DESC")
                && text.lines().any(|line| line == "  fix failing auth flow")
                && ["failed", "paused", "running"]
                    .into_iter()
                    .all(|status| text.contains(status))
                && !text.contains(":reasoning"),
            "{text}"
        );
    }

    #[test]
    fn agents_table_wraps_collapsed_description_to_width() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut agent = agent_with_status("long-desc", AgentStatus::Idle, TurnPhase::Idle, 1_000);
        agent.description = Some(
            "this description starts\nwith pasted\tcontent and keeps going across enough words to fill the first line, then the second line, then the third line, and finally more preview text that must be truncated because agent cards only show a bounded activity summary instead of the entire prompt or attached reference content"
                .to_owned(),
        );
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![agent],
            now,
        );
        let text = render_agents_text(&snapshot, now, 72);

        let description_lines: Vec<_> =
            text.lines().filter(|line| line.starts_with("  ")).collect();
        assert!(
            text.lines()
                .all(|line| unicode_width::UnicodeWidthStr::width(line) <= 72)
                && !description_lines.is_empty()
                && description_lines.len() <= 3
                && description_lines
                    .join(" ")
                    .contains("this description starts with pasted content")
                && description_lines
                    .last()
                    .is_some_and(|line| line.ends_with('…')),
            "{text}"
        );
    }

    #[test]
    fn agents_table_separates_descriptionless_cards() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut first = agent_with_status("first", AgentStatus::Idle, TurnPhase::Idle, 1_000);
        first.name = Some("alpha".to_owned());
        first.name_explicit = true;
        let mut second = agent_with_status("second", AgentStatus::Idle, TurnPhase::Idle, 1_000);
        second.name = Some("beta".to_owned());
        second.name_explicit = true;
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![first, second],
            now,
        );

        let text = render_agents_text(&snapshot, now, 120);
        let lines: Vec<_> = text.lines().collect();
        let first = lines
            .iter()
            .position(|line| line.starts_with("@alpha"))
            .expect("alpha row");
        let second = lines
            .iter()
            .position(|line| line.starts_with("@beta"))
            .expect("beta row");
        let earlier = first.min(second);

        assert_eq!(first.abs_diff(second), 2, "{text}");
        assert!(lines[earlier + 1].is_empty(), "{text}");
        assert!(!text.ends_with("\n\n"), "{text}");
    }

    #[test]
    fn agents_table_groups_lanes_with_theme_and_team_context() {
        let now = Timestamp::from_second(2_000).unwrap();
        let auth_path = Some("/repo/worktrees/auth-refresh");
        let mut external = agent_in_lane("external", None, None, None);
        external.status = AgentStatus::Failed;
        let mut snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/repo/main")),
            vec![
                agent_in_lane("planner", Some("auth-refresh"), auth_path, Some("forge")),
                agent_in_lane("coder", Some("auth-refresh"), auth_path, Some("forge")),
                agent_in_lane("stray", Some("auth-refresh"), auth_path, None),
                agent_in_lane("docs", Some("docs"), Some("/repo/main"), None),
                external,
                agent_in_lane(
                    "repeated",
                    Some("rimz/forge"),
                    Some("/repo/main"),
                    Some("forge"),
                ),
                agent_in_lane(
                    "mixed-one",
                    Some("mixed"),
                    Some("/repo/main"),
                    Some("forge"),
                ),
                agent_in_lane(
                    "mixed-two",
                    Some("mixed"),
                    Some("/repo/main"),
                    Some("review"),
                ),
            ],
            now,
        )
        .with_project_root(Some(PathBuf::from("/repo/main")));

        let refs = snapshot.agents.iter().collect::<Vec<_>>();
        let (auth_key, auth_label, auth_kind) = {
            let group = rimz::store::snapshot::group_live_agents_by_worktree(&refs, &snapshot)
                .into_iter()
                .find(|group| group.label == "auth-refresh")
                .unwrap();
            (group.key, group.label, group.kind)
        };
        snapshot.worktree_groups.push(
            serde_json::from_value(serde_json::json!({
                "key": auth_key,
                "label": auth_label,
                "kind": auth_kind,
                "status_counts": [],
                "rows": [],
                "pr_number": 91,
                "pr_state": "open",
                "ci": "passing"
            }))
            .unwrap(),
        );

        let text = render_agents_text(&snapshot, now, 120);
        assert!(text.contains("⑂ auth-refresh · forge team #91 ✓"), "{text}");
        assert!(text.contains("# docs"), "{text}");
        assert!(text.contains("external"), "{text}");
        assert!(
            !text.lines().next().unwrap_or_default().contains("CHANNEL"),
            "{text}"
        );
        for lane in ["rimz/forge", "# mixed"] {
            let header = text.lines().find(|line| line.contains(lane)).unwrap();
            assert!(!header.contains("team"), "{text}");
        }

        let theme = ThemeConfig {
            glyphs: ThemeGlyphsConfig {
                set: Some("nerd_font".to_owned()),
                ..ThemeGlyphsConfig::default()
            },
            ..ThemeConfig::default()
        };
        let text = render_agents_text_with_theme(&snapshot, now, 120, &theme);
        assert!(text.contains("\u{e0a0} auth-refresh"), "{text}");
        assert!(text.contains("#91 \u{f058}"), "{text}");
        assert!(text.contains("\u{f292} docs"), "{text}");
    }

    #[test]
    fn show_placement_includes_pr_state_and_ci() {
        let agent = agent_in_lane(
            "coder",
            Some("feature"),
            Some("/repo/worktrees/feature"),
            None,
        );
        let peers = [&agent];
        let report = super::report::build_entry(
            &agent,
            None,
            Some(super::report::PrInfo {
                number: Some(91),
                state: rimz::store::snapshot::WorktreePrState::Open,
                ci: Some(rimz::store::snapshot::WorktreeCi::Failing),
            }),
            &peers,
            None,
            Timestamp::UNIX_EPOCH,
            super::report::ReportOverrides::default(),
        );
        let mut out = anstream::StripStream::new(Vec::new());
        super::show::render_placement_section(&mut out, &report).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();

        assert!(
            text.contains("pr:") && text.contains("#91 open · ci failing"),
            "{text}"
        );
    }

    #[test]
    fn show_activity_projects_phase_only_for_active_turns() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut active =
            agent_with_status("active", AgentStatus::Running, TurnPhase::Acting, 1_000);
        active.description = Some("ship\nwide\tfix".to_owned());
        let mut idle = agent_with_status("idle", AgentStatus::Idle, TurnPhase::Idle, 1_000);
        let stop = rimz::agents::IdleStop {
            after_secs: 1_200,
            requested_at: Timestamp::from_second(900).unwrap(),
            requested_by: Some("@lead".to_owned()),
        };
        active.idle_stop = Some(rimz::agents::PendingIdleStop {
            stop: stop.clone(),
            due_at: None,
        });
        idle.idle_stop = Some(rimz::agents::PendingIdleStop {
            stop,
            due_at: Some(Timestamp::from_second(2_200).unwrap()),
        });
        let report = |agent: &AgentState| {
            let peers = [agent];
            super::report::build_entry(
                agent,
                None,
                None,
                &peers,
                None,
                now,
                super::report::ReportOverrides::default(),
            )
        };
        let active = report(&active);
        let idle = report(&idle);

        let mut active_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut active_out, &active, None, false, now)
            .expect("render active activity");
        let active_text = String::from_utf8(active_out.into_inner()).expect("utf8");
        assert!(
            active_text
                .lines()
                .any(|line| line.contains("status:") && line.contains("running"))
                && active_text
                    .lines()
                    .any(|line| line.contains("phase:") && line.contains("acting"))
                && active_text.contains("description:   ship wide fix"),
            "{active_text}"
        );
        assert!(
            active_text
                .lines()
                .any(|line| line.contains("idle_stop:") && line.ends_with("after 20m idle (@lead)")),
            "a busy agent's clock is not running: {active_text}"
        );
        assert!(
            serde_json::to_value(&active).unwrap()["idle_stop"]
                .get("due_at")
                .is_none()
        );

        let mut idle_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut idle_out, &idle, None, false, now)
            .expect("render idle activity");
        let idle_text = String::from_utf8(idle_out.into_inner()).expect("utf8");
        assert!(idle_text.contains("status:"), "{idle_text}");
        assert!(!idle_text.contains("phase:"), "{idle_text}");
        assert!(
            idle_text.lines().any(|line| line.contains("idle_stop:")
                && line.ends_with("after 20m idle, in 4m (@lead)")),
            "{idle_text}"
        );
        assert_eq!(
            serde_json::to_value(&idle).unwrap()["idle_stop"],
            serde_json::json!({
                "after_secs": 1_200, "requested_at": "1970-01-01T00:15:00Z",
                "requested_by": "@lead", "due_at": "1970-01-01T00:36:40Z"
            })
        );

        let mut native_wait = agent_with_status(
            "droid-wait",
            AgentStatus::Running,
            TurnPhase::Reasoning,
            1_000,
        );
        let mut context = rimz::agents::AgentContext::new("droid", now);
        context.settle = Some(rimz::agents::TurnSettle::new(
            Timestamp::from_second(1_010).unwrap(),
            rimz::agents::TurnSettleOutcome::NativeWait,
        ));
        native_wait.context = Some(context);
        let native_wait = report(&native_wait);
        let mut native_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut native_out, &native_wait, None, false, now)
            .expect("render native wait activity");
        let native_text = String::from_utf8(native_out.into_inner()).expect("utf8");
        assert!(native_text.contains("waiting"), "{native_text}");
        assert!(!native_text.contains("phase:"), "{native_text}");
    }
}

mod automation {
    use super::*;

    #[test]
    fn create_on_miss_launches_kinds_and_agent_profiles_but_not_commands() {
        let profiles = planner_profiles();

        assert!(is_launchable_type("codex", &profiles));
        assert!(is_launchable_type("planner", &profiles));
        assert!(!is_launchable_type("vim", &profiles));
        assert!(!is_launchable_type("swift-otter", &profiles));
    }
}

#[test]
fn provider_binding_debug_redacts_account_key() {
    let binding = |key: &str| {
        rimz::agents::ProviderAccountBinding::decode(&format!(
            r#"{{"scope":{{"kind":"sub_provider","provider":"alibaba","variant":"international"}},"account_key":"{key}"}}"#
        ))
        .expect("binding")
    };
    let expected = binding("owner");
    assert!(!format!("{expected:?}").contains("owner"));
}

#[test]
fn wait_style_labels_only_plural_or_any_results() {
    assert!(matches!(
        wait::WaitStyle::new(1, false, true),
        wait::WaitStyle::Single { json: true }
    ));
    assert_eq!(
        wait::WaitStyle::new(2, false, true),
        wait::WaitStyle::All { json: true },
    );
    assert_eq!(
        wait::WaitStyle::new(1, true, false),
        wait::WaitStyle::Any { json: false },
    );
}

#[test]
fn plural_wait_block_prints_completed_answer() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-wait")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        PathBuf::from("/tmp/rimz-wait"),
    );
    record.status = RunStatus::Completed;
    record.last_message = Some("child answer\n".to_owned());
    let outcome = wait::TargetOutcome {
        name: "calm-fox".to_owned(),
        payload: wait::TerminalPayload::Run(Box::new(record)),
    };
    let mut out = Vec::new();
    let mut err = Vec::new();

    wait::print_wait_block(
        &mut out,
        &mut err,
        &outcome,
        crate::cli::render::prose::Prose::Raw,
    )
    .unwrap();

    assert_eq!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string(),
        "--- calm-fox ---\nchild answer\n\n"
    );
    assert!(err.is_empty());
}

#[test]
fn plural_wait_block_marks_failure_and_prints_forensics() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-wait")),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        PathBuf::from("/tmp/rimz-wait"),
    );
    record.status = RunStatus::Failed;
    record.failure_tail = Some("provider failed".to_owned());
    let outcome = wait::TargetOutcome {
        name: "bright-owl".to_owned(),
        payload: wait::TerminalPayload::Run(Box::new(record)),
    };
    let mut out = Vec::new();
    let mut err = Vec::new();

    wait::print_wait_block(
        &mut out,
        &mut err,
        &outcome,
        crate::cli::render::prose::Prose::Raw,
    )
    .unwrap();

    assert_eq!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string(),
        "--- bright-owl (failed) ---\n\n"
    );
    let err = anstream::adapter::strip_str(&String::from_utf8(err).unwrap()).to_string();
    assert!(err.starts_with("--- bright-owl (failed) ---\n"));
    assert!(err.contains("rimz: run failed (exit 1)"));
    assert!(err.contains("provider failed"));
}

#[test]
fn plural_wait_block_for_agent_payload_prints_last_message() {
    let agent = agent_with_status(
        "agent_0123456789abcdef0123456789abcdef",
        AgentStatus::Idle,
        TurnPhase::Idle,
        1_000,
    );
    let outcome = wait::TargetOutcome {
        name: "quiet-lynx".to_owned(),
        payload: wait::TerminalPayload::Agent {
            agent: Box::new(agent),
            last_message: Some("reviewed; two findings".to_owned()),
        },
    };
    let mut out = Vec::new();
    let mut err = Vec::new();

    wait::print_wait_block(
        &mut out,
        &mut err,
        &outcome,
        crate::cli::render::prose::Prose::Raw,
    )
    .unwrap();

    assert_eq!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string(),
        "--- quiet-lynx ---\nreviewed; two findings\n\n"
    );
    assert!(err.is_empty());
    assert_eq!(
        serde_json::to_value(outcome.entry()).unwrap()["last_message"],
        "reviewed; two findings"
    );
}

#[test]
fn agent_wait_settles_on_this_turns_final_message_or_after_grace() {
    use rimz::agents::TurnCompletion;
    use rimz::transcript::{TranscriptEntry, TranscriptKind};
    use std::time::Instant;

    let started = Timestamp::from_second(1_000).unwrap();
    let entry = |second| {
        TranscriptEntry::new(
            Timestamp::from_second(second).unwrap(),
            AgentKind::new_unchecked("codex"),
            AgentSessionId::from("peer"),
            TranscriptKind::Assistant,
            format!("answer at {second}"),
        )
    };
    let now = Instant::now();
    let mut seen = None;

    assert_eq!(
        wait::settle_agent_turn(
            TurnCompletion::Open,
            Some(started),
            Some(entry(1_001)),
            &mut seen,
            (now, None)
        ),
        None
    );
    assert_eq!(
        wait::settle_agent_turn(
            TurnCompletion::Completed,
            Some(started),
            Some(entry(999)),
            &mut seen,
            (now, None)
        ),
        None
    );
    assert_eq!(
        wait::settle_agent_turn(
            TurnCompletion::Completed,
            Some(started),
            Some(entry(1_001)),
            &mut seen,
            (now, None)
        ),
        Some(Some("answer at 1001".to_owned()))
    );
    assert_eq!(
        wait::settle_agent_turn(
            TurnCompletion::Failed,
            Some(started),
            Some(entry(999)),
            &mut None,
            (now, None)
        ),
        Some(None)
    );

    let mut seen = None;
    assert_eq!(
        wait::settle_agent_turn(
            TurnCompletion::Completed,
            Some(started),
            None,
            &mut seen,
            (now, None)
        ),
        None
    );
    assert_eq!(
        wait::settle_agent_turn(
            TurnCompletion::Completed,
            Some(started),
            None,
            &mut seen,
            (now + Duration::from_secs(3), None)
        ),
        Some(None)
    );
    let mut seen = None;
    assert_eq!(
        wait::settle_agent_turn(
            TurnCompletion::Completed,
            Some(started),
            None,
            &mut seen,
            (now, Some(now))
        ),
        Some(None)
    );
}

#[test]
fn plural_wait_json_entry_includes_last_message() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-wait")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        PathBuf::from("/tmp/rimz-wait"),
    );
    record.status = RunStatus::Completed;
    record.last_message = Some("finished review".to_owned());
    let outcome = wait::TargetOutcome {
        name: "calm-fox".to_owned(),
        payload: wait::TerminalPayload::Run(Box::new(record)),
    };

    let value = serde_json::to_value(outcome.entry()).unwrap();

    assert_eq!(value["last_message"], "finished review");
}

fn render_agents_text(
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    now: Timestamp,
    max_width: usize,
) -> String {
    render_agents_text_with_theme(snapshot, now, max_width, &ThemeConfig::default())
}

fn render_agents_text_with_theme(
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    now: Timestamp,
    max_width: usize,
    theme: &ThemeConfig,
) -> String {
    let agents: Vec<&AgentState> = snapshot.agents.iter().collect();
    let mut out = anstream::StripStream::new(Vec::new());
    render_agents_table(&mut out, snapshot, &agents, now, max_width, theme)
        .expect("render agents table");
    String::from_utf8(out.into_inner()).expect("utf8")
}

fn agent_with_turn_error(
    mut agent: AgentState,
    class: TurnErrorClass,
    at: i64,
    label: &str,
) -> AgentState {
    let at = Timestamp::from_second(at).unwrap();
    let mut context = rimz::agents::AgentContext::new(&agent.kind.to_string(), at);
    context.turn_error = Some(AgentTurnError {
        class,
        at,
        label: Some(label.to_owned()),
    });
    agent.context = Some(context);
    agent
}

fn agent_in_lane(
    id: &str,
    channel: Option<&str>,
    worktree: Option<&str>,
    team: Option<&str>,
) -> AgentState {
    let mut agent = agent_with_status(id, AgentStatus::Idle, TurnPhase::Idle, 1_000);
    agent.channel = channel.map(ToOwned::to_owned);
    agent.worktree_path = worktree.map(ToOwned::to_owned);
    agent.worktree_branch = worktree.map(|_| "main".to_owned());
    agent.team = team.map(ToOwned::to_owned);
    agent
}

fn agent_with_status(id: &str, status: AgentStatus, phase: TurnPhase, activity: i64) -> AgentState {
    let at = Timestamp::from_second(activity).unwrap();
    AgentState {
        status,
        phase,
        worktree_path: Some("/tmp/rimz-agents-table".to_owned()),
        worktree_branch: Some("main".to_owned()),
        ..rimz::testkit::agent_state("claude", id, at)
    }
}
