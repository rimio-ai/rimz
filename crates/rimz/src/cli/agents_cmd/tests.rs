use super::exec::*;
use super::launch::*;
use super::*;
use clap::Parser;
use rimz::bridge::{ExpectedRunFrame, RunWakeOutcome};
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

#[test]
fn agents_launch_parses_spec_prompt_and_worktree_name() {
    let parsed = AgentsHarness::try_parse_from([
        "rimz",
        "claude,codex+term",
        "fix the tests",
        "--worktree=docs",
        "--no-focus",
    ])
    .expect("parse agents launch");

    assert!(parsed.args.command.is_none());
    assert_eq!(parsed.args.spec.as_deref(), Some("claude,codex+term"));
    assert_eq!(parsed.args.prompt.as_deref(), Some("fix the tests"));
    assert_eq!(parsed.args.worktree.as_deref(), Some("docs"));
    assert!(parsed.args.no_focus);
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
        &[],
    );

    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [Cell::Agent { args, .. }] = column.rows.as_slice() else {
        panic!("single agent cell");
    };
    assert!(args.is_empty());
}

#[test]
fn explicit_interactive_mode_applies_even_when_alias_added_args() {
    let mut layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: vec!["--model".to_owned(), "gpt-5-codex".to_owned()],
        mode: None,
    });

    apply_launch_mode_and_passthrough(
        &mut layout,
        interactive_permission_mode_from_flags(false, true)
            .unwrap()
            .map(LaunchModeApplication::explicit),
        &[],
    );

    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [Cell::Agent { args, .. }] = column.rows.as_slice() else {
        panic!("single agent cell");
    };
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
    });

    apply_launch_mode_and_passthrough(
        &mut layout,
        Some(LaunchModeApplication::implicit_default(
            PermissionMode::Auto,
        )),
        &[],
    );

    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [Cell::Agent { args, mode, .. }] = column.rows.as_slice() else {
        panic!("single agent cell");
    };
    assert_eq!(args, &yolo_args);
    assert_eq!(*mode, Some(PermissionMode::Yolo));
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
    });

    apply_launch_mode_and_passthrough(
        &mut layout,
        Some(LaunchModeApplication::explicit(PermissionMode::Yolo)),
        &[],
    );

    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [Cell::Agent { args, mode, .. }] = column.rows.as_slice() else {
        panic!("single agent cell");
    };
    assert_eq!(args, &auto_args);
    assert_eq!(*mode, Some(PermissionMode::Auto));
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
