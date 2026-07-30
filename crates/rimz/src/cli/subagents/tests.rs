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
            "spec": "claude",
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
        r#"[{"spec":"codex","prompt":"one","timeout":"5m"}]"#,
        &flagged,
        &defaults,
    )
    .expect("task timeout");
    assert_eq!(task[0].timeout, Some(Duration::from_secs(5 * 60)));

    let flag = parse_fanout_launches(r#"[{"spec":"codex","prompt":"one"}]"#, &flagged, &defaults)
        .expect("flag timeout");
    assert_eq!(flag[0].timeout, Some(Duration::from_secs(10 * 60)));

    let Some(SubagentsSubcmd::Fanout(unflagged)) = parse(&["rimz", "fanout"]).command else {
        panic!("fanout command");
    };
    let config = parse_fanout_launches(
        r#"[{"spec":"codex","prompt":"one"}]"#,
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
        parse_fanout_launches(r#"[{"spec":"codex","name":"auth"}]"#, &fanout, &defaults)
            .expect_err("missing prompt");
    assert!(format!("{missing_prompt:#}").contains("task 1 (auth)"));
    assert!(format!("{missing_prompt:#}").contains("prompt from the parent"));

    let conflicting_prompt = parse_fanout_launches(
        r#"[{"spec":"codex","prompt":"inline","prompt_file":"prompt.md"}]"#,
        &fanout,
        &defaults,
    )
    .expect_err("conflicting prompt sources");
    assert!(format!("{conflicting_prompt:#}").contains("both `prompt` and `prompt_file`"));

    let duplicate = parse_fanout_launches(
        r#"[
            {"spec":"codex","prompt":"one","name":"auth"},
            {"spec":"claude","prompt":"two","name":"auth"}
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
        r#"[{{"spec":"codex","prompt_file":{}}}]"#,
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
fn available_specs_include_kinds_profiles_and_commands_but_not_teams() {
    let mut config = rimz::config::MachineConfig::default();
    config.agents.profiles.0.insert(
        "planner".to_owned(),
        rimz::config::Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: Some("fable".to_owned()),
            effort: Some("high".to_owned()),
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        },
    );
    config.agents.profiles.0.insert(
        "claude".to_owned(),
        rimz::config::Profile {
            agent: "claude".to_owned(),
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

    let specs = available_specs(&config);

    assert!(specs.iter().any(|entry| entry.source == "kind"));
    assert!(specs.iter().any(|entry| {
        entry.name == "planner"
            && entry.source == "profile"
            && entry.detail() == "claude · fable@high"
    }));
    assert!(
        specs
            .iter()
            .any(|entry| entry.name == "mytool" && entry.source == "command")
    );
    assert_eq!(
        specs.iter().filter(|entry| entry.name == "claude").count(),
        1
    );
    assert!(
        specs
            .iter()
            .any(|entry| entry.name == "claude" && entry.source == "profile")
    );
    assert!(!specs.iter().any(|entry| entry.name == "review"));
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
fn specs_are_the_only_user_shell_subcommand() {
    let error = require_agent_caller(false).expect_err("human caller");
    assert!(error.to_string().contains("rimz agents <spec>"));

    for argv in [
        &["rimz", "launch", "codex", "review"][..],
        &["rimz", "fanout"],
        &["rimz", "list"],
        &["rimz", "wait"],
        &["rimz", "stop", "--all"],
        &["rimz"],
    ] {
        let args = parse(argv);
        assert!(command_is_agent_only(args.command.as_ref()), "{argv:?}");
    }
    for argv in [&["rimz", "specs"][..], &["rimz", "types"]] {
        let args = parse(argv);
        assert!(!command_is_agent_only(args.command.as_ref()), "{argv:?}");
        assert!(matches!(args.command, Some(SubagentsSubcmd::Specs { .. })));
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
        rimz::harness::run::PermissionMode::Auto,
        "review".to_owned(),
        PathBuf::from("/tmp/subagent-wait"),
    );
    finished_run.agent_id = Some(finished.agent_id.clone());
    finished_run.agent_name = finished.name.clone();
    finished_run.status = rimz::harness::run::RunStatus::Completed;
    let mut running_run = rimz::harness::run::RunRecord::new(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/subagent-wait")),
        rimz::ids::AgentKind::new_unchecked("codex"),
        rimz::harness::run::PermissionMode::Auto,
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
