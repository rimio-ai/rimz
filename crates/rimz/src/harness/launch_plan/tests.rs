use super::*;
use crate::harness::launch::{ExecAction, ExecIdentity, ProviderAccountState};
use crate::ids::AgentKind;

fn exec_launch_reminders(
    request: &ExecRequest,
    machine: &crate::config::MachineConfig,
    project: &Path,
    config: &Path,
) -> LaunchReminders {
    let effective = crate::config::effective::load_with_roots(machine, project, config);
    reminders(request, effective.as_ref().ok(), &machine.agents.commands).0
}

fn request(kind: &str, action: ExecAction) -> ExecRequest {
    ExecRequest {
        kind: AgentKind::new_unchecked(kind),
        action,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        skills: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        identity: ExecIdentity::default(),
    }
}

fn action_with_args(action: &str, args: Vec<String>) -> ExecAction {
    match action {
        "launch" => ExecAction::Launch {
            prompt: None,
            extra_args: args,
        },
        "resume" => ExecAction::Resume {
            session_id: "session".to_owned(),
            extra_args: args,
        },
        "fork" => ExecAction::Fork {
            session_id: "session".to_owned(),
            extra_args: args,
        },
        _ => unreachable!("test action is known"),
    }
}

#[test]
fn broken_effective_config_skips_launch_reminder() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let project_config = project.path().join(".rimz");
    std::fs::create_dir_all(&project_config).expect("create project config dir");
    std::fs::write(
        project_config.join("config.toml"),
        "[profiles.child]\nagent = \"unknown-base\"\n",
    )
    .expect("write broken project config");
    crate::trust::grant_with_roots(project.path(), config.path()).expect("trust project");
    let machine_config = crate::config::MachineConfig::default();
    let request = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );

    let reminders = exec_launch_reminders(&request, &machine_config, project.path(), config.path());
    assert!(reminders.subagent_catalog.is_none());
    assert!(reminders.team.is_none());
    assert!(reminders.model);
}

#[test]
fn profile_model_reminder_flag_reaches_launch_reminders() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let machine_config: crate::config::MachineConfig = toml::from_str(
        r#"
        [agents.profiles.quiet]
        agent = "claude"
        model-reminder = false
        [agents.profiles.loud]
        agent = "quiet"
        [subagents.profiles.quiet]
        agent = "claude"
        model-reminder = true
        [subagents.profiles.child]
        agent = "claude"
        model-reminder = false
    "#,
    )
    .expect("config");
    for (profile, subagent, expected) in [
        (Some("quiet"), false, false),
        (Some("loud"), false, true),
        (Some("quiet"), true, true),
        (Some("child"), true, false),
        (None, false, true),
        (None, true, true),
    ] {
        let mut request = request(
            "claude",
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        );
        request.identity.params.profile = profile.map(str::to_owned);
        request.subagent = subagent;
        let reminders =
            exec_launch_reminders(&request, &machine_config, project.path(), config.path());
        assert_eq!(
            reminders.model, expected,
            "{profile:?}, subagent={subagent}"
        );
    }
}

#[test]
fn resolves_team_for_launch_context_from_effective_config() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let mut machine_config = crate::config::MachineConfig::default();
    machine_config.agents.teams.0.insert(
        "forge".to_owned(),
        crate::config::Team {
            roles: vec![
                crate::config::RoleBinding {
                    signals: Vec::new(),
                    owns: Vec::new(),
                    compact_on_handoff: false,
                    auto_compact: None,
                    role: "planner".to_owned(),
                    profile: "claude".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                },
                crate::config::RoleBinding {
                    signals: Vec::new(),
                    owns: Vec::new(),
                    compact_on_handoff: false,
                    auto_compact: None,
                    role: "coder".to_owned(),
                    profile: "codex".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                },
            ],
            leader: Some("planner".to_owned()),
            layout: None,
            scratch_files: vec!["blackboard.md".to_owned()],
            stages: Vec::new(),
        },
    );
    let mut request = request(
        "codex",
        ExecAction::Resume {
            session_id: "session".to_owned(),
            extra_args: Vec::new(),
        },
    );
    request.identity.params = crate::agents::LaunchParams {
        team: Some("forge".to_owned()),
        role: Some("coder".to_owned()),
        channel: Some("feature".to_owned()),
        ..crate::agents::LaunchParams::default()
    };

    let reminders = exec_launch_reminders(&request, &machine_config, project.path(), config.path());
    let team = reminders.team.expect("team");
    assert_eq!(team.leader.as_deref(), Some("planner"));
    assert_eq!(
        team.roles
            .iter()
            .map(|role| role.role.as_str())
            .collect::<Vec<_>>(),
        ["planner", "coder"]
    );
    assert_eq!(team.scratch_files, ["blackboard.md"]);
}

#[test]
fn omits_team_context_without_team_identity_and_catalog_for_children() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let machine_config = crate::config::MachineConfig::default();
    let request = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    let reminders = exec_launch_reminders(&request, &machine_config, project.path(), config.path());
    assert!(reminders.subagent_catalog.is_some());
    assert!(reminders.team.is_none());
    assert!(reminders.model);

    let mut child = request;
    child.subagent = true;
    let reminders = exec_launch_reminders(&child, &machine_config, project.path(), config.path());
    assert!(reminders.subagent_catalog.is_none());
    assert!(reminders.team.is_none());
    assert!(reminders.model);
}

#[test]
fn materialized_prompt_replaces_raw_prompt_args_for_every_action() {
    let materialized = MaterializedSystemPrompt {
        args: vec![
            "--system-prompt-file".to_owned(),
            "/runtime/prompt/sys.composed.md".to_owned(),
        ],
        env: BTreeMap::new(),
    };
    for action in ["launch", "resume", "fork"] {
        let mut request = request(
            "claude",
            action_with_args(
                action,
                vec![
                    "--system-prompt-file".to_owned(),
                    "/raw.md".to_owned(),
                    "--verbose".to_owned(),
                ],
            ),
        );
        apply_materialized_system_prompt(&mut request, &materialized);
        assert_eq!(
            request.action.extra_args(),
            [
                "--verbose",
                "--system-prompt-file",
                "/runtime/prompt/sys.composed.md"
            ],
            "{action}"
        );
    }

    let mut codex = request(
        "codex",
        action_with_args(
            "resume",
            vec![
                "-c".to_owned(),
                "model_instructions_file=/raw.md".to_owned(),
            ],
        ),
    );
    apply_materialized_system_prompt(
        &mut codex,
        &MaterializedSystemPrompt {
            args: vec![
                "-c".to_owned(),
                "model_instructions_file=/runtime/prompt/sys.composed.md".to_owned(),
            ],
            env: BTreeMap::new(),
        },
    );
    assert_eq!(
        codex.action.extra_args(),
        [
            "-c",
            "model_instructions_file=/runtime/prompt/sys.composed.md"
        ]
    );

    let mut pi = request(
        "pi",
        action_with_args(
            "fork",
            vec!["--system-prompt".to_owned(), "raw text".to_owned()],
        ),
    );
    apply_materialized_system_prompt(
        &mut pi,
        &MaterializedSystemPrompt {
            args: vec![
                "--system-prompt".to_owned(),
                "base text\n\nfragment text\n".to_owned(),
            ],
            env: BTreeMap::new(),
        },
    );
    assert_eq!(
        pi.action.extra_args(),
        ["--system-prompt", "base text\n\nfragment text\n"]
    );
}

#[test]
fn prompt_environment_reaches_qwen_without_entering_argv() {
    let project = tempfile::tempdir().expect("project");
    let base = project.path().join("base.md");
    let fragment = project.path().join("fragment.md");
    std::fs::write(&base, "base\n\n").unwrap();
    std::fs::write(&fragment, "fragment").unwrap();
    let machine = crate::config::MachineConfig::default();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    for kind in ["qwen", "claude", "pi", "amp"] {
        let root = project.path().join(kind);
        let workspace_id = crate::WorkspaceId::from_project_root(&root);
        let runtime = RuntimePaths::under(workspace_id.clone(), &root).unwrap();
        let state = StatePaths::under(workspace_id, &root).unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        request.identity.params.model = Some("test-model".to_owned());
        if kind != "amp" {
            request.system_prompt_file = Some(base.clone());
            request.append_system_prompt_files = vec![fragment.clone()];
        }
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            bwrap: None,
            ambient_env: &BTreeMap::new(),
        })
        .unwrap();
        assert!(!root.exists(), "{kind} compilation must not write");
        assert!(plan.sandbox.is_none());
        let reminder = plan.process().reminder.as_ref().expect("model reminder");
        assert!(reminder.contains("<system_reminder>"));
        assert_eq!(
            plan.reminder_channel.is_some(),
            matches!(kind, "claude" | "qwen")
        );
        if kind == "amp" {
            continue;
        }
        assert_eq!(plan.prompt.composed.as_deref(), Some("base\n\nfragment\n"));
        let artifact = plan.prompt.artifact.as_ref().unwrap();
        assert!(!artifact.exists());
        let process = plan.process();
        if kind == "qwen" {
            assert_eq!(
                process.env.get("QWEN_SYSTEM_MD"),
                artifact.to_str().map(str::to_owned).as_ref()
            );
            assert!(!process.provider_argv.iter().any(|arg| arg.contains("sys.")));
        }
        if kind == "claude" {
            assert!(
                process.provider_argv.windows(2).any(
                    |args| args[0] == "--system-prompt-file" && Path::new(&args[1]) == artifact
                )
            );
            assert!(
                process
                    .provider_argv
                    .windows(2)
                    .any(|args| args[0] == "--append-system-prompt" && &args[1] == reminder)
            );
        }
        apply(&plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(artifact).unwrap(),
            plan.prompt.composed.as_deref().unwrap()
        );
        assert!(!state.tmp_dir.exists());
    }
}
