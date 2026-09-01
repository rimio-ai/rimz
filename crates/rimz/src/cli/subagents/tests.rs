use std::path::PathBuf;

use clap::Parser;
use jiff::Timestamp;

use super::*;

#[derive(Debug, Parser)]
struct Harness {
    #[command(flatten)]
    args: SubagentsArgs,
}

#[derive(Debug, Parser)]
struct AgentsHarness {
    #[command(flatten)]
    args: agents_cmd::AgentsArgs,
}

fn parse(argv: &[&str]) -> SubagentsArgs {
    Harness::try_parse_from(argv)
        .expect("parse subagents command")
        .args
}

#[test]
fn launch_implies_supervised_background_defaults() {
    let args = parse(&["rimz", "claude", "review this", "--effort", "high"]);
    let launch = args
        .launch
        .into_agent_launch(&rimz::config::SubagentsConfig::default())
        .expect("launch payload");
    assert!(launch.self_cleanup_on_completion);
    let agents = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "review this",
        "--effort",
        "high",
        "-p",
        "--bg",
        "--timeout",
        "30m",
    ])
    .expect("parse equivalent agents launch")
    .args;
    let mut agents = agents;
    agents.launch.self_cleanup_on_completion = true;
    assert!(launch.subagent);
    assert_eq!(launch.prompt.as_deref(), Some("review this"));
    agents.launch.subagent = true;

    assert_eq!(launch, agents.launch);
}

#[test]
fn waited_launch_still_desugars_to_a_background_run() {
    let args = parse(&["rimz", "claude", "review this", "--wait"]);
    assert_eq!(args.launch.wait, Some(None));
    let launch = args
        .launch
        .into_agent_launch(&rimz::config::SubagentsConfig::default())
        .expect("launch payload");
    let agents = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "review this",
        "-p",
        "--bg",
        "--timeout",
        "30m",
    ])
    .expect("parse equivalent agents launch")
    .args;
    let mut agents = agents;
    agents.launch.self_cleanup_on_completion = true;
    assert!(launch.subagent);
    assert_eq!(launch.prompt.as_deref(), Some("review this"));
    agents.launch.subagent = true;

    assert_eq!(launch, agents.launch);
}

#[test]
fn wait_uses_an_optional_equals_duration() {
    let args = parse(&["rimz", "claude", "review this", "--wait=5m"]);
    assert_eq!(args.launch.wait, Some(Some(Duration::from_secs(5 * 60))));
    assert!(Harness::try_parse_from(["rimz", "claude", "review this", "--wait", "5m"]).is_err());

    let swallowed = parse(&["rimz", "claude", "--wait", "5m"]);
    assert_eq!(swallowed.launch.prompt.as_deref(), Some("5m"));
    let error = swallowed
        .launch
        .into_agent_launch(&rimz::config::SubagentsConfig::default())
        .expect_err("space-form duration must not become the prompt");
    assert!(error.to_string().contains("did you mean `--wait=5m`?"));
}

#[test]
fn waited_single_launch_accepts_json_in_both_forms() {
    let bare = parse(&["rimz", "claude", "review this", "--wait", "--json"]);
    assert_eq!(bare.launch.wait, Some(None));
    assert!(bare.json);

    let explicit = parse(&[
        "rimz",
        "launch",
        "claude",
        "review this",
        "--wait",
        "--json",
    ]);
    let Some(SubagentsSubcmd::Launch(explicit)) = explicit.command else {
        panic!("explicit launch");
    };
    assert_eq!(explicit.launch.wait, Some(None));
    assert!(explicit.json);
}

#[test]
fn fanout_task_matches_the_single_launch_surface() {
    let fanout = parse(&[
        "rimz",
        "fanout",
        "tasks.json",
        "--timeout",
        "10m",
        "--keep",
        "--wait=2m",
        "--json",
    ]);
    let Some(SubagentsSubcmd::Fanout(fanout)) = fanout.command else {
        panic!("fanout command");
    };
    assert_eq!(fanout.file, Some(PathBuf::from("tasks.json")));
    assert_eq!(fanout.wait, Some(Some(Duration::from_secs(2 * 60))));
    assert!(fanout.json);
    let launches = parse_fanout_launches(
        r#"[{
            "profile": "claude",
            "prompt": "review this",
            "name": "auth-review",
            "model": "opus",
            "agent": "reviewer",
            "effort": "high",
            "timeout": "5m",
            "max_turns": 4,
            "description": "checks auth"
        }]"#,
        &fanout,
        &rimz::config::SubagentsConfig::default(),
    )
    .expect("fanout launch");
    let agents = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "review this",
        "--name",
        "auth-review",
        "--model",
        "opus",
        "--agent",
        "reviewer",
        "--effort",
        "high",
        "--timeout",
        "5m",
        "--max-turns",
        "4",
        "--description",
        "checks auth",
        "--keep",
        "-p",
        "--bg",
    ])
    .expect("parse equivalent agents launch")
    .args;
    let mut agents = agents;
    agents.launch.self_cleanup_on_completion = true;
    assert!(launches[0].subagent);
    assert_eq!(launches[0].prompt.as_deref(), Some("review this"));
    agents.launch.subagent = true;

    assert_eq!(launches, vec![agents.launch]);
}

#[test]
fn fanout_timeout_precedence_is_task_then_flag_then_config() {
    let Some(SubagentsSubcmd::Fanout(flagged)) =
        parse(&["rimz", "fanout", "--timeout", "10m"]).command
    else {
        panic!("fanout command");
    };
    let defaults = rimz::config::SubagentsConfig {
        timeout: "20m".to_owned(),
    };

    let task = parse_fanout_launches(
        r#"[{"profile":"codex","prompt":"one","timeout":"5m"}]"#,
        &flagged,
        &defaults,
    )
    .expect("task timeout");
    assert_eq!(task[0].timeout, Some(Duration::from_secs(5 * 60)));

    let flag = parse_fanout_launches(
        r#"[{"profile":"codex","prompt":"one"}]"#,
        &flagged,
        &defaults,
    )
    .expect("flag timeout");
    assert_eq!(flag[0].timeout, Some(Duration::from_secs(10 * 60)));

    let Some(SubagentsSubcmd::Fanout(unflagged)) = parse(&["rimz", "fanout"]).command else {
        panic!("fanout command");
    };
    let config = parse_fanout_launches(
        r#"[{"profile":"codex","prompt":"one"}]"#,
        &unflagged,
        &defaults,
    )
    .expect("config timeout");
    assert_eq!(config[0].timeout, Some(Duration::from_secs(20 * 60)));
}

#[test]
fn fanout_validates_the_whole_task_list_before_launch() {
    let Some(SubagentsSubcmd::Fanout(fanout)) = parse(&["rimz", "fanout"]).command else {
        panic!("fanout command");
    };
    let defaults = rimz::config::SubagentsConfig::default();

    let empty = parse_fanout_launches("[]", &fanout, &defaults).expect_err("empty task list");
    assert!(empty.to_string().contains("at least one task"));

    let missing_prompt =
        parse_fanout_launches(r#"[{"profile":"codex","name":"auth"}]"#, &fanout, &defaults)
            .expect_err("missing prompt");
    assert!(format!("{missing_prompt:#}").contains("task 1 (auth)"));
    assert!(format!("{missing_prompt:#}").contains("prompt from the parent"));

    let conflicting_prompt = parse_fanout_launches(
        r#"[{"profile":"codex","prompt":"inline","prompt_file":"prompt.md"}]"#,
        &fanout,
        &defaults,
    )
    .expect_err("conflicting prompt sources");
    assert!(format!("{conflicting_prompt:#}").contains("both `prompt` and `prompt_file`"));

    let duplicate = parse_fanout_launches(
        r#"[
            {"profile":"codex","prompt":"one","name":"auth"},
            {"profile":"claude","prompt":"two","name":"auth"}
        ]"#,
        &fanout,
        &defaults,
    )
    .expect_err("duplicate name");
    assert!(
        duplicate
            .to_string()
            .contains("task 2 repeats child name `auth`")
    );
}

#[test]
fn fanout_rejects_the_retired_spec_key() {
    let Some(SubagentsSubcmd::Fanout(fanout)) = parse(&["rimz", "fanout"]).command else {
        panic!("fanout command");
    };
    let error = parse_fanout_launches(
        r#"[{"spec":"codex","prompt":"one"}]"#,
        &fanout,
        &rimz::config::SubagentsConfig::default(),
    )
    .expect_err("retired spec key");
    let error = format!("{error:#}");

    assert!(error.contains("unknown field `spec`"));
    assert!(error.contains("`profile`"));
}

#[test]
fn unattended_launch_flags_are_rejected() {
    for args in [
        &["rimz", "claude", "review this", "--ask"][..],
        &["rimz", "claude", "review this", "--yolo"],
        &["rimz", "claude", "review this", "--budget", "5"],
    ] {
        assert!(
            Harness::try_parse_from(args).is_err(),
            "{args:?} must not be accepted"
        );
    }
}

#[test]
fn prompt_files_resolve_for_single_launch_and_fanout() {
    let dir = tempfile::tempdir().expect("prompt tempdir");
    let prompt_path = dir.path().join("review.md");
    std::fs::write(&prompt_path, "review the parser\n").expect("write prompt");
    let prompt_path = prompt_path.to_string_lossy();

    let args = parse(&["rimz", "codex", "--prompt-file", &prompt_path]);
    let launch = args
        .launch
        .into_agent_launch(&rimz::config::SubagentsConfig::default())
        .expect("file-backed launch");
    assert_eq!(launch.prompt.as_deref(), Some("review the parser"));

    let Some(SubagentsSubcmd::Fanout(fanout)) = parse(&["rimz", "fanout"]).command else {
        panic!("fanout command");
    };
    let raw = format!(
        r#"[{{"profile":"codex","prompt_file":{}}}]"#,
        serde_json::to_string(prompt_path.as_ref()).expect("json path")
    );
    let launches = parse_fanout_launches(&raw, &fanout, &rimz::config::SubagentsConfig::default())
        .expect("file-backed fanout");
    assert_eq!(launches[0].prompt.as_deref(), Some("review the parser"));

    assert!(
        Harness::try_parse_from([
            "rimz",
            "codex",
            "inline prompt",
            "--prompt-file",
            prompt_path.as_ref(),
        ])
        .is_err()
    );
}

#[test]
fn available_profiles_include_profiles_and_commands_but_not_kinds_or_teams() {
    let mut config = rimz::config::MachineConfig::default();
    config.agents.profiles.0.insert(
        "agent-only".to_owned(),
        rimz::config::Profile {
            agent: "claude".to_owned(),
            description: None,
            subagents: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        },
    );
    config.subagents.profiles.0.insert(
        "planner".to_owned(),
        rimz::config::Profile {
            agent: "claude".to_owned(),
            description: Some("Plans supervised work".to_owned()),
            subagents: None,
            mode: None,
            model: Some("fable".to_owned()),
            effort: Some("high".to_owned()),
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        },
    );
    config.subagents.profiles.0.insert(
        "claude".to_owned(),
        rimz::config::Profile {
            agent: "claude".to_owned(),
            description: None,
            subagents: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        },
    );
    config
        .agents
        .commands
        .0
        .insert("mytool".to_owned(), "mytool --chat".to_owned());
    config
        .agents
        .teams
        .0
        .insert("review".to_owned(), rimz::config::Team::default());

    let profiles = crate::cli::profile_report::available_profiles(
        &config.subagents.profiles,
        &config.agents.commands,
        &rimz::config::AgentSpecSources::default(),
        rimz::config::effective::ProfileScope::Subagents,
    );

    assert!(!profiles.iter().any(|entry| entry.source == "kind"));
    assert!(profiles.iter().any(|entry| {
        entry.name == "planner"
            && entry.source == "profile"
            && entry.agent.as_deref() == Some("claude")
            && entry.model.as_deref() == Some("fable")
            && entry.effort.as_deref() == Some("high")
            && entry.description.as_deref() == Some("Plans supervised work")
    }));
    assert!(!profiles.iter().any(|entry| entry.name == "agent-only"));
    assert!(
        profiles
            .iter()
            .any(|entry| entry.name == "mytool" && entry.source == "command")
    );
    assert_eq!(
        profiles
            .iter()
            .filter(|entry| entry.name == "claude")
            .count(),
        1
    );
    assert!(
        profiles
            .iter()
            .any(|entry| entry.name == "claude" && entry.source == "profile")
    );
    assert!(!profiles.iter().any(|entry| entry.name == "review"));
}

#[test]
fn launch_requires_parent_prompt() {
    let args = parse(&["rimz", "claude"]);
    let error = args
        .launch
        .into_agent_launch(&rimz::config::SubagentsConfig::default())
        .expect_err("missing prompt");
    assert!(error.to_string().contains("prompt from the parent"));
}

#[test]
fn list_and_profiles_are_the_user_shell_subcommands() {
    let error = require_agent_caller(false).expect_err("human caller");
    assert!(error.to_string().contains("rimz subagents list"));
    assert!(error.to_string().contains("rimz agents <spec>"));

    for argv in [
        &["rimz", "launch", "codex", "review"][..],
        &["rimz", "fanout"],
        &["rimz", "wait"],
        &["rimz", "stop", "--all"],
    ] {
        let args = parse(argv);
        assert!(command_is_agent_only(&args), "{argv:?}");
    }
    for argv in [
        &["rimz", "list"][..],
        &["rimz", "list", "--json"],
        &["rimz"],
    ] {
        let args = parse(argv);
        assert!(!command_is_agent_only(&args), "{argv:?}");
    }
    let args = parse(&["rimz", "profiles", "--path"]);
    assert!(!command_is_agent_only(&args));
    assert!(matches!(
        args.command,
        Some(SubagentsSubcmd::Profiles {
            json: false,
            path: true
        })
    ));
    for command in ["specs", "types"] {
        let args = parse(&["rimz", command]);
        assert!(args.command.is_none());
        assert_eq!(args.launch.profile.as_deref(), Some(command));
        assert!(command_is_agent_only(&args));
    }
}

#[test]
fn default_wait_keeps_finished_supervised_children() {
    let mut finished =
        rimz::agents::AgentState::stub("codex", "finished", rimz::agents::AgentStatus::Success);
    finished.name = Some("swift-otter".to_owned());
    finished.ended_at = Some(Timestamp::now());
    let mut running =
        rimz::agents::AgentState::stub("codex", "running", rimz::agents::AgentStatus::Running);
    running.name = Some("bright-owl".to_owned());
    let untracked =
        rimz::agents::AgentState::stub("claude", "interactive", rimz::agents::AgentStatus::Idle);
    let children = vec![&finished, &running, &untracked];

    let mut finished_run = rimz::harness::run::RunRecord::new(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/subagent-wait")),
        rimz::ids::AgentKind::new_unchecked("codex"),
        rimz::agents::PermissionMode::Auto,
        "review".to_owned(),
        PathBuf::from("/tmp/subagent-wait"),
    );
    finished_run.agent_id = Some(finished.agent_id.clone());
    finished_run.agent_name = finished.name.clone();
    finished_run.status = rimz::harness::run::RunStatus::Completed;
    let mut running_run = rimz::harness::run::RunRecord::new(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/subagent-wait")),
        rimz::ids::AgentKind::new_unchecked("codex"),
        rimz::agents::PermissionMode::Auto,
        "implement".to_owned(),
        PathBuf::from("/tmp/subagent-wait"),
    );
    running_run.agent_id = Some(running.agent_id.clone());
    running_run.agent_name = running.name.clone();
    let runs = [finished_run, running_run];

    assert_eq!(
        wait_references(&children, &runs, &[], false).expect("default join"),
        vec!["swift-otter", "bright-owl"]
    );
    assert_eq!(
        wait_references(&children, &runs, &[], true).expect("default any"),
        vec!["bright-owl"]
    );
}

#[test]
fn child_reports_name_each_parent_and_channel() {
    let mut planner =
        rimz::agents::AgentState::stub("claude", "planner", rimz::agents::AgentStatus::Idle);
    planner.name = Some("planner".to_owned());
    planner.name_explicit = true;
    planner.channel = Some("feat-x".to_owned());
    let mut coder =
        rimz::agents::AgentState::stub("codex", "coder", rimz::agents::AgentStatus::Idle);
    coder.name = Some("coder".to_owned());
    coder.name_explicit = true;
    coder.channel = Some("feat-x".to_owned());
    let mut first =
        rimz::agents::AgentState::stub("codex", "child-a", rimz::agents::AgentStatus::Success);
    first.name = Some("swift-otter".to_owned());
    first.parent_agent_id = Some(planner.agent_id.clone());
    first.parent_agent_kind = Some(planner.kind.clone());
    first.launch_depth = Some(1);
    first.channel = Some("feat-x".to_owned());
    let mut second =
        rimz::agents::AgentState::stub("claude", "child-b", rimz::agents::AgentStatus::Running);
    second.name = Some("bright-owl".to_owned());
    second.parent_agent_id = Some(coder.agent_id.clone());
    second.parent_agent_kind = Some(coder.kind.clone());
    second.launch_depth = Some(1);
    second.channel = Some("feat-x".to_owned());
    let mut orphan = second.clone();
    orphan.agent_id = "child-orphan".into();
    orphan.parent_agent_id = Some("missing-parent".into());
    let mut run = rimz::harness::run::RunRecord::new(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/subagent-list")),
        first.kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "review".to_owned(),
        PathBuf::from("/tmp/subagent-list"),
    );
    run.agent_id = Some(first.agent_id.clone());
    run.status = rimz::harness::run::RunStatus::Completed;
    let agents = [planner, coder, first, second, orphan];
    let children = [&agents[2], &agents[3], &agents[4]];

    let reports = child_reports(&agents, &children, &[run]);

    assert_eq!(reports[0].parent, "@planner");
    assert_eq!(reports[1].parent, "@coder");
    assert_eq!(reports[2].parent, "@missing-parent");
    assert_eq!(reports[0].channel.as_deref(), Some("feat-x"));
    assert_eq!(reports[0].run_status.as_deref(), Some("completed"));
    assert!(reports[1].run_status.is_none());
    let value = serde_json::to_value(&reports[0]).expect("serialize child report");
    assert_eq!(value["parent"], "@planner");
    assert_eq!(value["channel"], "feat-x");
}

#[test]
fn child_report_json_includes_parent_and_omits_an_unknown_channel() {
    let report = ChildReport {
        name: "swift-otter".to_owned(),
        handle: "@swift-otter".to_owned(),
        parent: "@planner".to_owned(),
        channel: None,
        kind: "codex".to_owned(),
        status: "running".to_owned(),
        description: None,
        run_id: None,
        run_status: None,
    };

    let value = serde_json::to_value(report).expect("serialize child report");

    assert_eq!(value["parent"], "@planner");
    assert!(value.get("channel").is_none());
}

#[test]
fn explicit_child_resolution_uses_the_shared_address_grammar() {
    let mut child =
        rimz::agents::AgentState::stub("codex", "child", rimz::agents::AgentStatus::Running);
    child.name = Some("swift-otter".to_owned());
    child.channel = Some("review".to_owned());

    assert_eq!(
        resolve_child_names(&[&child], &["@swift-otter#review".to_owned()])
            .expect("qualified child"),
        vec![&child]
    );
    assert!(
        resolve_child_names(&[&child], &["@swift-otter#other".to_owned()]).is_err(),
        "wrong-channel child must not resolve"
    );
}

#[test]
fn lifecycle_verbs_parse() {
    assert!(matches!(
        parse(&["rimz", "fanout", "tasks.json", "--wait"]).command,
        Some(SubagentsSubcmd::Fanout(FanoutArgs {
            wait: Some(None),
            file: Some(_),
            ..
        }))
    ));
    assert!(matches!(
        parse(&["rimz", "wait", "swift-otter", "--any"]).command,
        Some(SubagentsSubcmd::Wait { any: true, .. })
    ));
    assert!(matches!(
        parse(&["rimz", "stop", "--all"]).command,
        Some(SubagentsSubcmd::Stop { all: true, .. })
    ));
}
