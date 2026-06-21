use super::exec::*;
use super::launch::*;
use super::*;
use clap::Parser;
use rimz::bridge::{ExpectedRunFrame, RunWakeOutcome};
use rimz::config::LaunchPlacement;
use rimz::ids::{AgentKind, WorkspaceId};
use rimz::run::{PermissionMode, RunRecord, RunStatus};

#[derive(Debug, Parser)]
struct ExecHarness {
    #[command(subcommand)]
    command: AgentsSubcmd,
}

#[derive(Debug, Parser)]
struct AgentsHarness {
    #[command(flatten)]
    args: AgentsArgs,
}

/// The launch args and mode of the sole agent cell in a single-column,
/// single-row layout — the shape every `apply_launch_*` test builds.
fn only_agent(layout: &LayoutSpec) -> (&[String], Option<PermissionMode>) {
    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [Cell::Agent { args, mode, .. }] = column.rows.as_slice() else {
        panic!("single agent cell");
    };
    (args, *mode)
}

#[test]
fn agents_launch_parses_spec_prompt_and_worktree_name() {
    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude,codex+term",
        "fix the tests",
        "--worktree=docs",
        "--bg",
    ])
    .expect("parse agents launch");

    assert!(parsed.args.command.is_none());
    assert_eq!(parsed.args.spec.as_deref(), Some("claude,codex+term"));
    assert_eq!(parsed.args.prompt.as_deref(), Some("fix the tests"));
    assert_eq!(parsed.args.worktree.as_deref(), Some("docs"));
    assert!(parsed.args.bg);
}

#[test]
fn agents_launch_accepts_space_separated_worktree() {
    let parsed = AgentsHarness::try_parse_from(["rimz", "peer", "--worktree", "unread-tune"])
        .expect("parse agents launch space-separated worktree");

    assert_eq!(parsed.args.spec.as_deref(), Some("peer"));
    assert_eq!(parsed.args.worktree.as_deref(), Some("unread-tune"));
    assert!(parsed.args.prompt.is_none());
}

#[test]
fn agents_launch_from_pr_parses_and_can_name_worktree() {
    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "codex",
        "--from-pr",
        "https://gitlab.com/org/repo/-/merge_requests/12",
        "--worktree",
        "review-12",
    ])
    .expect("parse agents launch from PR");

    assert_eq!(parsed.args.spec.as_deref(), Some("codex"));
    assert_eq!(parsed.args.worktree.as_deref(), Some("review-12"));
    assert_eq!(
        parsed.args.from_pr,
        Some(rimz::forge::PrTarget {
            number: 12,
            forge: Some(rimz::forge::Forge::GitLab)
        })
    );
}

#[test]
fn agents_list_verb_does_not_parse_as_launch_spec() {
    let parsed =
        AgentsHarness::try_parse_from(["rimz", "list", "--json"]).expect("parse agents list");
    assert!(matches!(
        parsed.args.command,
        Some(AgentsSubcmd::List { json: true, .. })
    ));
}

#[test]
fn agents_list_all_conflicts_with_worktree_filter() {
    let err = AgentsHarness::try_parse_from(["rimz", "list", "--all", "--worktree", "docs"])
        .expect_err("all channels and one channel conflict");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn agents_bare_json_parses_as_list_flag() {
    let parsed = AgentsHarness::try_parse_from(["rimz", "--json"]).expect("parse agents json");
    assert!(parsed.args.command.is_none());
    assert!(parsed.args.spec.is_none());
    assert!(parsed.args.json);
}

#[test]
fn full_launch_env_marks_agent_kind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let adapter = rimz::agents::find_adapter("claude").expect("claude adapter");
    let env = full_agent_launch_env(
        dir.path(),
        adapter,
        AgentLaunchEnvIdentity {
            agent_name: Some("swift-otter"),
            agent_profile: Some("planner"),
            agent_role: Some("coder"),
            agent_model: Some("gpt-5.5"),
            agent_effort: Some("xhigh"),
            ..AgentLaunchEnvIdentity::default()
        },
    )
    .expect("launch env");

    assert_eq!(
        env.get(rimz::run::ENV_AGENT_KIND).map(String::as_str),
        Some("claude")
    );
    assert_eq!(
        env.get(rimz::run::ENV_AGENT_NAME).map(String::as_str),
        Some("swift-otter")
    );
    assert_eq!(
        env.get(rimz::run::ENV_AGENT_PROFILE).map(String::as_str),
        Some("planner")
    );
    assert_eq!(
        env.get(rimz::run::ENV_AGENT_ROLE).map(String::as_str),
        Some("coder")
    );
    assert_eq!(
        env.get(rimz::run::ENV_AGENT_MODEL).map(String::as_str),
        Some("gpt-5.5")
    );
    assert_eq!(
        env.get(rimz::run::ENV_AGENT_EFFORT).map(String::as_str),
        Some("xhigh")
    );
}

#[test]
fn pane_command_stamps_agent_role() {
    let cell = Cell::Agent {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        profile: Some("claude-planner".to_owned()),
        role: Some("planner".to_owned()),
        model: Some("claude-sonnet".to_owned()),
        effort: Some("high".to_owned()),
    };

    let pane = pane_cmd_with_name(
        &cell,
        Path::new("/usr/bin/rimz"),
        Path::new("/tmp/project"),
        None,
        false,
        false,
        None,
    )
    .expect("pane command");

    assert_eq!(
        pane.argv,
        [
            "/usr/bin/rimz",
            "agents",
            "exec",
            "claude",
            "--close-pane-on-exit",
            "--agent-profile",
            "claude-planner",
            "--agent-role",
            "planner",
            "--agent-model",
            "claude-sonnet",
            "--agent-effort",
            "high",
        ]
    );
}

#[test]
fn in_place_pane_command_leaves_user_pane_open() {
    let cell = Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        profile: None,
        role: None,
        model: None,
        effort: None,
    };

    let pane = pane_cmd_with_name(
        &cell,
        Path::new("/usr/bin/rimz"),
        Path::new("/tmp/project"),
        None,
        false,
        true,
        None,
    )
    .expect("pane command");

    assert_eq!(pane.argv, ["/usr/bin/rimz", "agents", "exec", "codex"]);
}

#[test]
fn agents_print_cluster_accepts_json_with_print_flag() {
    let parsed = AgentsHarness::try_parse_from(["rimz", "claude", "hi", "--json"])
        .expect("parse agents launch json");
    assert_eq!(parsed.args.spec.as_deref(), Some("claude"));
    assert!(parsed.args.json);
    assert!(!parsed.args.print);

    let parsed = AgentsHarness::try_parse_from(["rimz", "claude", "hi", "-p", "--json"])
        .expect("parse agents print json");
    assert!(parsed.args.print);
    assert!(parsed.args.json);
}

#[test]
fn print_output_and_input_formats_parse_and_require_print() {
    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "hi",
        "-p",
        "--output-format",
        "stream-json",
    ])
    .expect("parse output-format");
    assert_eq!(parsed.args.output_format, Some(OutputFormat::StreamJson));

    let parsed =
        AgentsHarness::try_parse_from(["rimz", "claude", "-p", "--input-format", "stream-json"])
            .expect("parse input-format");
    assert_eq!(parsed.args.input_format, Some(InputFormat::StreamJson));

    let parsed = AgentsHarness::try_parse_from(["rimz", "claude", "-p", "explain"])
        .expect("parse prompt after print flag");
    assert_eq!(parsed.args.prompt.as_deref(), Some("explain"));

    // The removed boolean is gone, and the format flags need `--print`.
    assert!(AgentsHarness::try_parse_from(["rimz", "claude", "hi", "-p", "--stream"]).is_err());
    assert!(
        AgentsHarness::try_parse_from(["rimz", "claude", "hi", "--output-format", "json"]).is_err()
    );
}

#[test]
fn effort_and_system_prompt_file_parse_and_require_spec() {
    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "hi",
        "--model",
        "opus",
        "--description",
        "port auth",
        "--effort",
        "high",
        "--system-prompt-file",
        "/abs/prompt.md",
        "--append-system-prompt-file",
        "/abs/append.md",
        "-p",
        "--max-turns",
        "3",
    ])
    .expect("parse shared launch params");
    assert_eq!(parsed.args.model.as_deref(), Some("opus"));
    assert_eq!(parsed.args.description.as_deref(), Some("port auth"));
    assert_eq!(parsed.args.effort.as_deref(), Some("high"));
    assert_eq!(
        parsed.args.system_prompt_file.as_deref(),
        Some(Path::new("/abs/prompt.md"))
    );
    assert_eq!(
        parsed.args.append_system_prompt_file.as_deref(),
        Some(Path::new("/abs/append.md"))
    );
    assert_eq!(parsed.args.max_turns, Some(3));

    let parsed = AgentsHarness::try_parse_from(["rimz", "claude", "-n", "swift-otter"])
        .expect("parse name short flag");
    assert_eq!(parsed.args.name.as_deref(), Some("swift-otter"));

    let parsed = AgentsHarness::try_parse_from(["rimz", "--effort", "high"])
        .expect("parse effort without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject effort");
    assert!(err.to_string().contains("require an agent spec"), "{err:#}");

    let parsed = AgentsHarness::try_parse_from(["rimz", "--model", "opus"]).expect("parse model");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject model");
    assert!(err.to_string().contains("require an agent spec"), "{err:#}");

    let parsed = AgentsHarness::try_parse_from(["rimz", "--description", "port auth"])
        .expect("parse description without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject description");
    assert!(err.to_string().contains("require an agent spec"), "{err:#}");

    let parsed =
        AgentsHarness::try_parse_from(["rimz", "--append-system-prompt-file", "/abs/append.md"])
            .expect("parse append prompt without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject append prompt");
    assert!(err.to_string().contains("require an agent spec"), "{err:#}");

    let parsed = AgentsHarness::try_parse_from(["rimz", "-p", "--max-turns", "3"])
        .expect("parse max turns without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject max turns");
    assert!(err.to_string().contains("require an agent spec"), "{err:#}");

    assert!(
        AgentsHarness::try_parse_from(["rimz", "claude", "hi", "--max-turns", "3"]).is_err(),
        "--max-turns is print-mode only"
    );
}

#[test]
fn system_prompt_file_resolves_a_file_and_rejects_a_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("prompt.md");
    std::fs::write(&file, "be concise").expect("write prompt");

    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "hi",
        "--model",
        "opus",
        "--system-prompt-file",
        file.to_str().expect("utf8 file path"),
    ])
    .expect("parse system-prompt-file");
    let preset = launch_override_preset(&parsed.args).expect("resolve prompt file");
    assert_eq!(preset.model.as_deref(), Some("opus"));
    assert_eq!(
        preset.system_prompt_file.as_deref(),
        Some(file.canonicalize().expect("canonicalize file").as_path())
    );

    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "hi",
        "--system-prompt-file",
        dir.path().to_str().expect("utf8 dir path"),
    ])
    .expect("parse system-prompt-file dir");
    let err = launch_override_preset(&parsed.args).expect_err("reject a directory");
    assert!(err.to_string().contains("is not a regular file"), "{err:#}");
}

#[test]
fn append_system_prompt_file_resolves_a_file_and_rejects_bad_paths() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("append.md");
    std::fs::write(&file, "follow project rules").expect("write prompt");

    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "hi",
        "--append-system-prompt-file",
        file.to_str().expect("utf8 file path"),
    ])
    .expect("parse append-system-prompt-file");
    let preset = launch_override_preset(&parsed.args).expect("resolve append prompt file");
    assert_eq!(
        preset.append_system_prompt_file.as_deref(),
        Some(file.canonicalize().expect("canonicalize file").as_path())
    );

    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "hi",
        "--append-system-prompt-file",
        dir.path().to_str().expect("utf8 dir path"),
    ])
    .expect("parse append-system-prompt-file dir");
    let err = launch_override_preset(&parsed.args).expect_err("reject a directory");
    assert!(err.to_string().contains("is not a regular file"), "{err:#}");

    let missing = dir.path().join("missing.md");
    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude",
        "hi",
        "--append-system-prompt-file",
        missing.to_str().expect("utf8 missing path"),
    ])
    .expect("parse missing append-system-prompt-file");
    let err = launch_override_preset(&parsed.args).expect_err("reject missing path");
    assert!(
        err.to_string()
            .contains("reading --append-system-prompt-file"),
        "{err:#}"
    );
}

#[test]
fn preset_renders_effort_per_adapter_and_fails_fast_for_pi() {
    let preset = rimz::agents::LaunchPreset {
        effort: Some("high".to_owned()),
        ..Default::default()
    };

    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));
    apply_launch_mode_and_passthrough(&mut layout, None, &preset, &[]).expect("claude effort");
    let (args, _) = only_agent(&layout);
    assert_eq!(args, &["--effort", "high"]);

    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
    apply_launch_mode_and_passthrough(&mut layout, None, &preset, &[]).expect("codex effort");
    let (args, _) = only_agent(&layout);
    assert_eq!(args, &["-c", "model_reasoning_effort=high"]);

    // Pi has no effort flag: the launch refuses at the unsupported cell.
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("pi")));
    let err = apply_launch_mode_and_passthrough(&mut layout, None, &preset, &[])
        .expect_err("pi rejects effort");
    assert!(
        err.to_string().contains("pi does not support --effort"),
        "{err:#}"
    );
}

#[test]
fn preset_renders_model_and_append_prompt_per_adapter() {
    let preset = rimz::agents::LaunchPreset {
        model: Some("opus".to_owned()),
        append_system_prompt_file: Some(Path::new("/abs/append.md").to_path_buf()),
        ..Default::default()
    };

    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));
    apply_launch_mode_and_passthrough(&mut layout, None, &preset, &[])
        .expect("claude model and append prompt");
    let (args, _) = only_agent(&layout);
    assert_eq!(
        args,
        &[
            "--model",
            "opus",
            "--append-system-prompt-file",
            "/abs/append.md"
        ]
    );

    let model_preset = rimz::agents::LaunchPreset {
        model: Some("gpt-5-codex".to_owned()),
        ..Default::default()
    };
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
    apply_launch_mode_and_passthrough(&mut layout, None, &model_preset, &[]).expect("codex model");
    let (args, _) = only_agent(&layout);
    assert_eq!(args, &["--model", "gpt-5-codex"]);

    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
    let err = apply_launch_mode_and_passthrough(&mut layout, None, &preset, &[])
        .expect_err("codex rejects append prompt");
    assert!(
        err.to_string()
            .contains("codex does not support --append-system-prompt-file"),
        "{err:#}"
    );

    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("pi")));
    let err = apply_launch_mode_and_passthrough(&mut layout, None, &preset, &[])
        .expect_err("pi rejects model first");
    assert!(
        err.to_string().contains("pi does not support --model"),
        "{err:#}"
    );

    let append_only = rimz::agents::LaunchPreset {
        append_system_prompt_file: Some(Path::new("/abs/append.md").to_path_buf()),
        ..Default::default()
    };
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("pi")));
    let err = apply_launch_mode_and_passthrough(&mut layout, None, &append_only, &[])
        .expect_err("pi rejects append prompt");
    assert!(
        err.to_string()
            .contains("pi does not support --append-system-prompt-file"),
        "{err:#}"
    );
}

#[test]
fn supervised_turn_limit_renders_per_adapter_and_fails_fast() {
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));
    apply_supervised_turn_limit(&mut layout, 3).expect("claude supports max turns");
    let (args, _) = only_agent(&layout);
    assert_eq!(args, &["--max-turns", "3"]);

    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
    let err = apply_supervised_turn_limit(&mut layout, 3).expect_err("codex rejects max turns");
    assert!(
        err.to_string()
            .contains("codex does not support --max-turns"),
        "{err:#}"
    );
}

#[test]
fn wait_stream_flags_have_the_run_style_conflict_matrix() {
    assert!(AgentsHarness::try_parse_from(["rimz", "wait", "codex", "--from-start"]).is_err());
    assert!(
        AgentsHarness::try_parse_from(["rimz", "wait", "codex", "--stream", "--json"]).is_err()
    );
}

#[test]
fn launch_flags_require_a_spec() {
    let parsed =
        AgentsHarness::try_parse_from(["rimz", "--worktree=docs"]).expect("parse worktree");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject worktree");
    assert!(err.to_string().contains("--worktree requires"), "{err:#}");

    let parsed = AgentsHarness::try_parse_from(["rimz", "--from-pr", "1"])
        .expect("parse from-pr without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject from-pr");
    assert!(err.to_string().contains("--from-pr requires"), "{err:#}");

    let parsed = AgentsHarness::try_parse_from(["rimz", "--", "term"])
        .expect("parse passthrough without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject passthrough");
    assert!(err.to_string().contains("missing agent spec"));

    let parsed =
        AgentsHarness::try_parse_from(["rimz", "--new-pane"]).expect("parse new-pane without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject new-pane");
    assert!(err.to_string().contains("require an agent spec"), "{err:#}");
}

#[test]
fn launch_placement_flags_parse_and_conflict() {
    let parsed =
        AgentsHarness::try_parse_from(["rimz", "claude", "--new-pane"]).expect("parse new-pane");
    assert!(parsed.args.new_pane);
    assert!(!parsed.args.new_tab);

    let parsed =
        AgentsHarness::try_parse_from(["rimz", "claude", "--new-tab"]).expect("parse new-tab");
    assert!(parsed.args.new_tab);

    assert!(
        AgentsHarness::try_parse_from(["rimz", "claude", "--new-pane", "--new-tab"]).is_err(),
        "--new-pane and --new-tab are mutually exclusive"
    );
}

#[test]
fn launch_placement_resolves_from_flags_policy_and_feasibility() {
    use Placement::{NewPane, NewTab, SamePane};

    // auto default: a single non-worktree agent with a launching pane → current pane.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Auto, false, true, true).unwrap(),
        SamePane
    );
    // auto: a worktree launch always opens a new tab.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Auto, true, true, true).unwrap(),
        NewTab
    );
    // auto: a multi-cell layout opens a new tab.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Auto, false, false, true).unwrap(),
        NewTab
    );
    // auto: no launching pane (run from outside the room) falls back to a new tab.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Auto, false, true, false).unwrap(),
        NewTab
    );
    // --new-tab forces a new tab even for a single non-worktree agent.
    assert_eq!(
        resolve_placement(true, false, LaunchPlacement::Auto, false, true, true).unwrap(),
        NewTab
    );
    // --new-pane forces a split for a single agent (worktree included).
    assert_eq!(
        resolve_placement(false, true, LaunchPlacement::Auto, true, true, true).unwrap(),
        NewPane
    );
    // placement "pane" splits a single non-worktree agent.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Pane, false, true, true).unwrap(),
        NewPane
    );
    // placement "pane" keeps a worktree launch in a new tab.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Pane, true, true, true).unwrap(),
        NewTab
    );
    // placement "pane" falls back to a new tab for a multi-cell layout.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Pane, false, false, true).unwrap(),
        NewTab
    );
    // placement "pane" falls back when there is no launching pane.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Pane, false, true, false).unwrap(),
        NewTab
    );
    // placement "tab" overrides the auto current-pane default.
    assert_eq!(
        resolve_placement(false, false, LaunchPlacement::Tab, false, true, true).unwrap(),
        NewTab
    );
}

#[test]
fn run_placement_splits_when_current_pane_is_available() {
    assert_eq!(run_placement(false, true), RunPlacement::Split);
}

#[test]
fn run_placement_opens_tab_when_forced() {
    assert_eq!(run_placement(true, true), RunPlacement::Tab);
}

#[test]
fn run_placement_opens_tab_without_current_pane() {
    assert_eq!(run_placement(false, false), RunPlacement::Tab);
}

#[test]
fn in_place_launch_downgrades_when_caller_pane_must_stay_available() {
    assert_eq!(
        apply_in_place_downgrade(Placement::SamePane, true, true),
        Placement::NewPane
    );
    assert_eq!(
        apply_in_place_downgrade(Placement::SamePane, false, false),
        Placement::NewPane
    );
    assert_eq!(
        apply_in_place_downgrade(Placement::NewTab, false, false),
        Placement::NewTab
    );
    assert_eq!(
        apply_in_place_downgrade(Placement::NewPane, true, false),
        Placement::NewPane
    );
}

#[test]
fn explicit_new_pane_fails_fast_when_infeasible() {
    let err = resolve_placement(false, true, LaunchPlacement::Auto, false, false, true)
        .expect_err("multi-cell new-pane");
    assert!(err.to_string().contains("single agent cell"), "{err:#}");

    let err = resolve_placement(false, true, LaunchPlacement::Auto, false, true, false)
        .expect_err("paneless new-pane");
    assert!(err.to_string().contains("inside the room"), "{err:#}");
}

#[test]
fn prompt_that_looks_like_another_spec_errors() {
    let profiles = rimz::config::ProfilesConfig::default();
    let commands = rimz::config::CommandsConfig::default();
    let layouts = rimz::config::TeamsConfig::default();
    let err = reject_prompt_that_looks_like_spec(
        Some("claude"),
        Some("codex"),
        &profiles,
        &commands,
        &layouts,
    )
    .expect_err("reject fan-out");
    assert!(
        err.to_string()
            .contains("did you mean `rimz agents claude,codex`"),
        "{err:#}"
    );
}

#[test]
fn interactive_launch_without_mode_keeps_native_agent_permissions() {
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));

    apply_launch_mode_and_passthrough(
        &mut layout,
        interactive_permission_mode_from_flags(false, false)
            .unwrap()
            .map(LaunchModeApplication::explicit),
        &rimz::agents::LaunchPreset::default(),
        &[],
    )
    .expect("apply launch options");

    let (args, _) = only_agent(&layout);
    assert!(args.is_empty());
}

#[test]
fn explicit_interactive_mode_applies_even_when_profile_added_args() {
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: vec!["--model".to_owned(), "gpt-5-codex".to_owned()],
        mode: None,
        system_prompt_file: None,
        profile: None,
        role: None,
        model: None,
        effort: None,
    });

    apply_launch_mode_and_passthrough(
        &mut layout,
        interactive_permission_mode_from_flags(false, true)
            .unwrap()
            .map(LaunchModeApplication::explicit),
        &rimz::agents::LaunchPreset::default(),
        &[],
    )
    .expect("apply launch options");

    let (args, _) = only_agent(&layout);
    assert!(
        args.iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
    );
}

#[test]
fn launch_override_preset_replaces_cell_model_and_effort_identity() {
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: vec!["--model".to_owned(), "profile-model".to_owned()],
        mode: None,
        system_prompt_file: None,
        profile: Some("codex-coder".to_owned()),
        role: Some("coder".to_owned()),
        model: Some("profile-model".to_owned()),
        effort: Some("medium".to_owned()),
    });

    apply_launch_mode_and_passthrough(
        &mut layout,
        None,
        &rimz::agents::LaunchPreset {
            model: Some("override-model".to_owned()),
            effort: Some("xhigh".to_owned()),
            ..rimz::agents::LaunchPreset::default()
        },
        &[],
    )
    .expect("apply launch options");

    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [
        Cell::Agent {
            args,
            model,
            effort,
            ..
        },
    ] = column.rows.as_slice()
    else {
        panic!("single agent cell");
    };
    assert!(args.contains(&"override-model".to_owned()), "{args:?}");
    assert!(
        args.contains(&"model_reasoning_effort=xhigh".to_owned()),
        "{args:?}"
    );
    assert_eq!(model.as_deref(), Some("override-model"));
    assert_eq!(effort.as_deref(), Some("xhigh"));
}

#[test]
fn supervised_default_mode_skips_cells_with_virtual_or_profile_mode() {
    let yolo_args = rimz::agents::find_adapter("codex")
        .expect("codex")
        .permission_args(PermissionMode::Yolo);
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: yolo_args.clone(),
        mode: Some(PermissionMode::Yolo),
        system_prompt_file: None,
        profile: None,
        role: None,
        model: None,
        effort: None,
    });

    apply_launch_mode_and_passthrough(
        &mut layout,
        Some(LaunchModeApplication::implicit_default(
            PermissionMode::Auto,
        )),
        &rimz::agents::LaunchPreset::default(),
        &[],
    )
    .expect("apply launch options");

    let (args, mode) = only_agent(&layout);
    assert_eq!(args, &yolo_args);
    assert_eq!(mode, Some(PermissionMode::Yolo));
}

#[test]
fn explicit_mode_skips_cells_with_virtual_or_profile_mode() {
    let auto_args = rimz::agents::find_adapter("claude")
        .expect("claude")
        .permission_args(PermissionMode::Auto);
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("claude"),
        args: auto_args.clone(),
        mode: Some(PermissionMode::Auto),
        system_prompt_file: None,
        profile: None,
        role: None,
        model: None,
        effort: None,
    });

    apply_launch_mode_and_passthrough(
        &mut layout,
        Some(LaunchModeApplication::explicit(PermissionMode::Yolo)),
        &rimz::agents::LaunchPreset::default(),
        &[],
    )
    .expect("apply launch options");

    let (args, mode) = only_agent(&layout);
    assert_eq!(args, &auto_args);
    assert_eq!(mode, Some(PermissionMode::Auto));
}

#[test]
fn generated_worktree_name_is_soft_agent_name_candidate() {
    let layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));

    let requests = launch_identity_requests(&layout, None, Some("docs")).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].name, AgentLaunchName::Soft("docs".to_owned()));
    assert_eq!(requests[0].profile, None);
    assert_eq!(requests[0].role, None);

    let requests = launch_identity_requests(&layout, None, Some("my_feature")).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].name,
        AgentLaunchName::Soft("my_feature".to_owned())
    );
}

#[test]
fn launch_identity_requests_carry_cell_profile_and_role() {
    let layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        profile: Some("codex-coder".to_owned()),
        role: Some("coder".to_owned()),
        model: Some("gpt-5-codex".to_owned()),
        effort: Some("high".to_owned()),
    });

    let requests = launch_identity_requests(&layout, None, None).unwrap();

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind.as_str(), "codex");
    assert_eq!(requests[0].profile.as_deref(), Some("codex-coder"));
    assert_eq!(requests[0].role.as_deref(), Some("coder"));
}

#[test]
fn explicit_agent_name_still_hard_fails_on_invalid() {
    let layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));

    assert!(
        launch_identity_requests(&layout, Some("my_feature"), None)
            .unwrap_err()
            .to_string()
            .contains("invalid agent name")
    );
}

#[test]
fn exec_subcommand_captures_agent_args_after_separator() {
    let parsed = ExecHarness::try_parse_from([
        "rimz",
        "exec",
        "codex",
        "--run-id",
        "run_0123456789abcdef0123456789abcdef",
        "--agent-name",
        "lucid-atlas",
        "--agent-role",
        "coder",
        "--agent-model",
        "gpt-5.5",
        "--agent-effort",
        "xhigh",
        "--launch-id",
        "launch_0123456789abcdef0123456789abcdef",
        "--exit-on-run-completion",
        "--close-pane-on-exit",
        "--worktree-path",
        "/x",
        "--prompt",
        "hi",
        "--",
        "--model",
        "gpt-5-codex",
    ])
    .expect("parse exec");

    let AgentsSubcmd::Exec(args) = parsed.command else {
        panic!("expected exec subcommand");
    };
    assert_eq!(args.kind, "codex");
    assert_eq!(
        args.run_id.as_ref().map(rimz::RunId::as_str),
        Some("run_0123456789abcdef0123456789abcdef")
    );
    assert_eq!(args.agent_name.as_deref(), Some("lucid-atlas"));
    assert_eq!(args.agent_role.as_deref(), Some("coder"));
    assert_eq!(args.agent_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(args.agent_effort.as_deref(), Some("xhigh"));
    assert_eq!(
        args.launch_id.as_deref(),
        Some("launch_0123456789abcdef0123456789abcdef")
    );
    assert!(args.exit_on_run_completion);
    assert!(args.close_pane_on_exit);
    assert_eq!(args.worktree_path, Some(PathBuf::from("/x")));
    assert_eq!(args.prompt.as_deref(), Some("hi"));
    assert_eq!(args.extra_args, ["--model", "gpt-5-codex"]);
}

#[test]
fn exec_subcommand_parses_a_resume_launch() {
    let parsed = ExecHarness::try_parse_from(["rimz", "exec", "claude", "--resume", "sess-1"])
        .expect("parse exec");
    let AgentsSubcmd::Exec(args) = parsed.command else {
        panic!("expected exec subcommand");
    };
    assert_eq!(args.kind, "claude");
    assert_eq!(args.resume.as_deref(), Some("sess-1"));

    // A resume rehydrates idle; a fresh-launch prompt cannot ride along.
    assert!(
        ExecHarness::try_parse_from([
            "rimz", "exec", "claude", "--resume", "sess-1", "--prompt", "hi",
        ])
        .is_err()
    );
}

fn bare_exec_args() -> ExecArgs {
    ExecArgs {
        kind: "codex".to_owned(),
        resume: None,
        run_id: None,
        agent_name: Some("lucid-atlas".to_owned()),
        agent_profile: None,
        agent_role: None,
        agent_model: None,
        agent_effort: None,
        launch_id: Some("launch_0123456789abcdef0123456789abcdef".to_owned()),
        exit_on_run_completion: false,
        close_pane_on_exit: false,
        worktree_path: None,
        prompt: None,
        extra_args: Vec::new(),
    }
}

#[test]
fn unsupervised_exec_replaces_the_wrapper_with_the_agent_tui() {
    let args = bare_exec_args();

    assert_eq!(should_exec_agent_directly(&args), cfg!(unix));
}

#[test]
fn exec_keeps_the_wrapper_when_it_owns_run_or_cleanup_work() {
    let mut args = bare_exec_args();
    args.run_id = Some(rimz::RunId::new());
    assert!(!should_exec_agent_directly(&args));

    let mut args = bare_exec_args();
    args.worktree_path = Some(PathBuf::from("/tmp/rimz-worktree"));
    assert!(!should_exec_agent_directly(&args));

    let mut args = bare_exec_args();
    args.exit_on_run_completion = true;
    args.close_pane_on_exit = true;
    assert!(!should_exec_agent_directly(&args));
}

#[test]
fn exec_records_end_trace_for_interactive_wrapper_exits() {
    let args = bare_exec_args();
    assert!(should_record_end_trace(&args));

    let mut args = bare_exec_args();
    args.exit_on_run_completion = true;
    assert!(!should_record_end_trace(&args));
}

#[test]
fn run_stop_should_cancel_only_live_runs() {
    for status in [RunStatus::Pending, RunStatus::Running] {
        assert!(run_stop_should_cancel(&run_record_with_status(status)));
    }
    for status in [
        RunStatus::Completed,
        RunStatus::Canceled,
        RunStatus::Failed,
        RunStatus::TimedOut,
    ] {
        assert!(!run_stop_should_cancel(&run_record_with_status(status)));
    }
}

fn run_record_with_status(status: RunStatus) -> RunRecord {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = status;
    record
}

#[tokio::test(flavor = "current_thread")]
async fn child_exit_marks_nonterminal_run_failed_and_wakes_waiter() {
    let state = tempfile::tempdir().expect("state dir");
    let runtime_root = tempfile::tempdir().expect("runtime dir");
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).expect("paths");
    let runtime =
        rimz::RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    paths.ensure_dirs().expect("state dirs");
    runtime.ensure_dirs().expect("runtime dirs");
    let record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    let run_id = record.run_id.clone();
    rimz::run::create(&paths, &record).expect("create run");
    let (sock, _sock_path) = rimz::bridge::bind_run(&runtime, &run_id).expect("bind run");
    let context = RunExecContext {
        run_id: run_id.clone(),
        paths: paths.clone(),
        runtime,
        session_name: "rimz-test".to_owned(),
    };

    fail_run_if_child_exited_first(&context, Duration::ZERO);

    let failed = rimz::run::load(&paths, &run_id).expect("load failed run");
    assert_eq!(failed.status, RunStatus::Failed);
    let outcome = rimz::bridge::wait_for_run_completion_owning(
        sock,
        ExpectedRunFrame {
            workspace_id,
            run_id,
        },
        Some(Duration::from_secs(1)),
    )
    .await
    .expect("run wait");
    assert_eq!(outcome, RunWakeOutcome::Completed(RunStatus::Failed));
}

fn agent_profile(prompt_file: Option<&std::path::Path>) -> rimz::config::ProfilesConfig {
    let mut profiles = rimz::config::ProfilesConfig::default();
    profiles.0.insert(
        "planner".to_owned(),
        rimz::config::Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: prompt_file.map(std::path::Path::to_path_buf),
            args: None,
        },
    );
    profiles
}

#[test]
fn create_on_miss_launches_kinds_and_agent_profiles_but_not_commands() {
    // A kind and an agent profile carry a kind to staff a channel; a command
    // name and a pet name do not, so `--create` refuses them.
    let profiles = agent_profile(None);

    assert!(is_launchable_type("codex", &profiles));
    assert!(is_launchable_type("planner", &profiles));
    assert!(!is_launchable_type("vim", &profiles));
    assert!(!is_launchable_type("swift-otter", &profiles));
}

#[test]
fn profile_launch_requires_its_system_prompt_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let present = dir.path().join("planner.md");
    std::fs::write(&present, "be terse").expect("write prompt");

    let profiles = agent_profile(Some(&present));
    let layout = rimz::agents_spec::resolve_spec(
        Some("planner"),
        &profiles,
        &rimz::config::CommandsConfig::default(),
        &rimz::config::TeamsConfig::default(),
    )
    .expect("resolve planner profile");

    // The cell names the profile; a present prompt file passes the launch gate.
    ensure_profile_prompt_files(&layout).expect("present prompt passes");

    // A missing prompt file fails the launch with the path to fix.
    let missing = dir.path().join("absent.md");
    let missing_layout = rimz::agents_spec::resolve_spec(
        Some("planner"),
        &agent_profile(Some(&missing)),
        &rimz::config::CommandsConfig::default(),
        &rimz::config::TeamsConfig::default(),
    )
    .expect("resolve missing planner profile");
    let err = ensure_profile_prompt_files(&missing_layout).expect_err("missing prompt fails");
    assert!(err.to_string().contains("system-prompt-file"));
}

#[test]
fn for_task_builds_a_blocking_supervised_turn() {
    let args = AgentsArgs::for_task(TaskRunArgs {
        spec: "claude-ping".to_owned(),
        prompt: Some("ping".to_owned()),
        worktree: Some("main".to_owned()),
        ask: false,
        yolo: false,
        effort: Some("low".to_owned()),
        system_prompt_file: None,
        timeout: None,
    });
    assert_eq!(
        args.spec.as_deref(),
        Some("claude-ping"),
        "the spec is carried exactly"
    );
    assert_eq!(args.prompt.as_deref(), Some("ping"), "the prompt is `ping`");
    assert_eq!(
        args.effort.as_deref(),
        Some("low"),
        "lowest effort primes cheaply"
    );
    assert_eq!(
        args.worktree.as_deref(),
        Some("main"),
        "the channel is carried"
    );
    assert!(args.print, "the ping is a supervised -p run");
    assert!(!args.bg, "a window-priming ping blocks until the turn ends");
    assert!(
        args.passthrough.is_empty(),
        "no passthrough flags are injected"
    );

    // No channel keeps the worktree unset, hosting the ping in the room itself.
    assert_eq!(
        AgentsArgs::for_task(TaskRunArgs {
            spec: "codex".to_owned(),
            prompt: Some("check status".to_owned()),
            worktree: None,
            ask: false,
            yolo: false,
            effort: None,
            system_prompt_file: None,
            timeout: None,
        })
        .worktree,
        None
    );
}
