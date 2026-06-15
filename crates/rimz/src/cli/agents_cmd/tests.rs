use super::exec::*;
use super::launch::*;
use super::*;
use clap::Parser;
use rimz::bridge::{ExpectedRunFrame, RunWakeOutcome};
use rimz::config::TabPlacement;
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
fn agents_list_verb_does_not_parse_as_launch_spec() {
    let parsed =
        AgentsHarness::try_parse_from(["rimz", "list", "--json"]).expect("parse agents list");
    assert!(matches!(
        parsed.args.command,
        Some(AgentsSubcmd::List { json: true, .. })
    ));
}

#[test]
fn agents_bare_json_parses_as_list_flag() {
    let parsed = AgentsHarness::try_parse_from(["rimz", "--json"]).expect("parse agents json");
    assert!(parsed.args.command.is_none());
    assert!(parsed.args.spec.is_none());
    assert!(parsed.args.json);
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
        "--effort",
        "high",
        "--system-prompt-file",
        "/abs/prompt.md",
    ])
    .expect("parse shared launch params");
    assert_eq!(parsed.args.effort.as_deref(), Some("high"));
    assert_eq!(
        parsed.args.system_prompt_file.as_deref(),
        Some(Path::new("/abs/prompt.md"))
    );

    let parsed = AgentsHarness::try_parse_from(["rimz", "--effort", "high"])
        .expect("parse effort without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject effort");
    assert!(
        err.to_string().contains("require an agent layout spec"),
        "{err:#}"
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
        "--system-prompt-file",
        file.to_str().expect("utf8 file path"),
    ])
    .expect("parse system-prompt-file");
    let preset = launch_override_preset(&parsed.args).expect("resolve prompt file");
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

    let parsed = AgentsHarness::try_parse_from(["rimz", "--", "term"])
        .expect("parse passthrough without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject passthrough");
    assert!(err.to_string().contains("missing agent layout spec"));

    let parsed =
        AgentsHarness::try_parse_from(["rimz", "--same-tab"]).expect("parse same-tab without spec");
    let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject same-tab");
    assert!(
        err.to_string().contains("require an agent layout spec"),
        "{err:#}"
    );
}

#[test]
fn tab_placement_flags_parse_and_conflict() {
    let parsed =
        AgentsHarness::try_parse_from(["rimz", "claude", "--same-tab"]).expect("parse same-tab");
    assert!(parsed.args.same_tab);
    assert!(!parsed.args.new_tab);

    let parsed =
        AgentsHarness::try_parse_from(["rimz", "claude", "--new-tab"]).expect("parse new-tab");
    assert!(parsed.args.new_tab);

    assert!(
        AgentsHarness::try_parse_from(["rimz", "claude", "--same-tab", "--new-tab"]).is_err(),
        "--same-tab and --new-tab are mutually exclusive"
    );
}

#[test]
fn tab_placement_resolves_from_flags_policy_and_feasibility() {
    use TabTarget::{NewTab, SameTab};

    // auto default: a single non-worktree agent with a launching pane → same tab.
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::Auto, false, true, true).unwrap(),
        SameTab
    );
    // auto: a worktree launch always opens a new tab.
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::Auto, true, true, true).unwrap(),
        NewTab
    );
    // auto: a multi-cell layout opens a new tab.
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::Auto, false, false, true).unwrap(),
        NewTab
    );
    // auto: no launching pane (run from outside the room) falls back to a new tab.
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::Auto, false, true, false).unwrap(),
        NewTab
    );
    // --new-tab forces a new tab even for a single non-worktree agent.
    assert_eq!(
        resolve_tab_placement(true, false, TabPlacement::Auto, false, true, true).unwrap(),
        NewTab
    );
    // --same-tab forces a same-tab split for a single agent (worktree included).
    assert_eq!(
        resolve_tab_placement(false, true, TabPlacement::Auto, true, true, true).unwrap(),
        SameTab
    );
    // config "new" overrides the auto same-tab default.
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::New, false, true, true).unwrap(),
        NewTab
    );
    // config "same" splits a single agent (ignoring the worktree default, like the flag).
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::Same, true, true, true).unwrap(),
        SameTab
    );
    // config "same" falls back to a new tab for a multi-cell layout.
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::Same, false, false, true).unwrap(),
        NewTab
    );
    // config "same" falls back when there is no launching pane.
    assert_eq!(
        resolve_tab_placement(false, false, TabPlacement::Same, false, true, false).unwrap(),
        NewTab
    );
}

#[test]
fn explicit_same_tab_fails_fast_when_infeasible() {
    let err = resolve_tab_placement(false, true, TabPlacement::Auto, false, false, true)
        .expect_err("multi-cell same-tab");
    assert!(err.to_string().contains("single agent cell"), "{err:#}");

    let err = resolve_tab_placement(false, true, TabPlacement::Auto, false, true, false)
        .expect_err("paneless same-tab");
    assert!(err.to_string().contains("inside the room"), "{err:#}");
}

#[test]
fn prompt_that_looks_like_another_spec_errors() {
    let aliases = rimz::config::AliasesConfig::default();
    let layouts = rimz::config::LayoutsConfig::default();
    let err = reject_prompt_that_looks_like_spec(Some("claude"), Some("codex"), &aliases, &layouts)
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
fn explicit_interactive_mode_applies_even_when_alias_added_args() {
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: vec!["--model".to_owned(), "gpt-5-codex".to_owned()],
        mode: None,
        alias: None,
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
fn supervised_default_mode_skips_cells_with_virtual_or_alias_mode() {
    let yolo_args = rimz::agents::find_adapter("codex")
        .expect("codex")
        .permission_args(PermissionMode::Yolo);
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: yolo_args.clone(),
        mode: Some(PermissionMode::Yolo),
        alias: None,
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
fn explicit_mode_skips_cells_with_virtual_or_alias_mode() {
    let auto_args = rimz::agents::find_adapter("claude")
        .expect("claude")
        .permission_args(PermissionMode::Auto);
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("claude"),
        args: auto_args.clone(),
        mode: Some(PermissionMode::Auto),
        alias: None,
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

    let requests = launch_identity_requests(&layout, None, Some("my_feature")).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].name,
        AgentLaunchName::Soft("my_feature".to_owned())
    );
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

fn agent_role(prompt_file: Option<&std::path::Path>) -> rimz::config::AliasesConfig {
    let mut aliases = rimz::config::AliasesConfig::default();
    aliases.0.insert(
        "planner".to_owned(),
        rimz::config::Alias::Agent {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: prompt_file.map(std::path::Path::to_path_buf),
            args: None,
        },
    );
    aliases
}

#[test]
fn create_on_miss_launches_kinds_and_agent_roles_but_not_command_aliases() {
    // A kind and an agent role carry a kind to staff a channel; a command alias
    // and a pet name do not, so `--create` refuses them.
    let mut aliases = agent_role(None);
    aliases.0.insert(
        "vim".to_owned(),
        rimz::config::Alias::Command("nvim -p".to_owned()),
    );

    assert!(is_launchable_type("codex", &aliases));
    assert!(is_launchable_type("planner", &aliases));
    assert!(!is_launchable_type("vim", &aliases));
    assert!(!is_launchable_type("swift-otter", &aliases));
}

#[test]
fn alias_launch_requires_its_system_prompt_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let present = dir.path().join("planner.md");
    std::fs::write(&present, "be terse").expect("write prompt");

    let layout = rimz::agents_spec::resolve_layout(
        Some("planner"),
        &agent_role(Some(&present)),
        &rimz::config::LayoutsConfig::default(),
    )
    .expect("resolve planner role");

    // The cell names the role; a present prompt file passes the launch gate.
    ensure_alias_prompt_files(&layout, &agent_role(Some(&present))).expect("present prompt passes");

    // A missing prompt file fails the launch with the path to fix.
    let missing = dir.path().join("absent.md");
    let err = ensure_alias_prompt_files(&layout, &agent_role(Some(&missing)))
        .expect_err("missing prompt fails the launch");
    assert!(err.to_string().contains("system-prompt-file"));
}
