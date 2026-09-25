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
        isolation_default: None,
        kind: AgentKind::new_unchecked(kind),
        action,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
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
fn host_settings_are_private_artifacts_written_only_on_apply() {
    use std::os::unix::fs::PermissionsExt;
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    let settings = project.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"env":{"ANTHROPIC_API_KEY":"sk-secret-123"}}"#,
    )
    .unwrap();
    let mut request = ExecRequest::bare_launch(
        AgentKind::new_unchecked("claude"),
        vec!["--settings".into(), settings.display().to_string()],
    );
    request.skills = Some(Vec::new());
    let plan = compile(LaunchPlanInputs {
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: None,
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        bwrap: None,
        ambient_env: &BTreeMap::new(),
    })
    .unwrap();
    let process = plan.process();
    assert!(!process.provider_argv.join(" ").contains("sk-secret-123"));
    assert!(!process.argv.join(" ").contains("sk-secret-123"));
    assert!(!format!("{process:?}").contains("sk-secret-123"));
    let values: Vec<_> = process
        .provider_argv
        .windows(2)
        .filter(|pair| pair[0] == "--settings")
        .collect();
    assert_eq!(values.len(), 1);
    let path = Path::new(&values[0][1]);
    assert_eq!(path.parent(), Some(runtime.prompt_dir().as_path()));
    assert!(!path.exists());
    std::fs::create_dir_all(runtime.prompt_dir()).unwrap();
    std::fs::set_permissions(runtime.prompt_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    apply(&plan).unwrap();
    let merged: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(merged["env"]["ANTHROPIC_API_KEY"], "sk-secret-123");
    assert!(merged["skillOverrides"].is_object());
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(runtime.prompt_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
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
    let mut machine_config = crate::config::MachineConfig::default();
    for (name, agent, model_reminder) in [("quiet", "claude", false), ("loud", "quiet", true)] {
        machine_config.agents.profiles.0.insert(
            name.to_owned(),
            crate::config::Profile {
                agent: agent.to_owned(),
                isolation: None,
                model_reminder: Some(model_reminder),
                description: None,
                subagents: None,
                mode: None,
                model: None,
                effort: None,
                budget: None,
                auto_compact: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                skills: None,
                args: None,
            },
        );
    }
    for (name, model_reminder) in [("quiet", true), ("child", false)] {
        machine_config.subagents.profiles.0.insert(
            name.to_owned(),
            crate::config::Profile {
                agent: "claude".to_owned(),
                isolation: None,
                model_reminder: Some(model_reminder),
                description: None,
                subagents: None,
                mode: None,
                model: None,
                effort: None,
                budget: None,
                auto_compact: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                skills: None,
                args: None,
            },
        );
    }
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
                    flip_compact: None,
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
                    flip_compact: None,
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
            scratch_files: Some(vec!["blackboard.md".to_owned()]),
            consensus_file: None,
            append_system_prompt_files: Vec::new(),
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
    assert_eq!(team.team.leader.as_deref(), Some("planner"));
    assert_eq!(
        team.team
            .roles
            .iter()
            .map(|role| role.role.as_str())
            .collect::<Vec<_>>(),
        ["planner", "coder"]
    );
    assert_eq!(team.team.scratch_patterns(), ["blackboard.md"]);
    // Every seat's agent is resolved here, where the effective profiles are in hand.
    let context = crate::harness::launch_context::team_launch_context(
        &request.identity.params,
        &request.action,
        &team,
        project.path(),
    )
    .expect("team context");
    assert!(
        crate::harness::launch_context::reminder(&context, Some("Codex GPT 6 Astra")).contains(
            "Seats: @planner runs on Claude; @coder (you) runs on Codex GPT 6 Astra. Resumed session"
        )
    );
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
        request.identity.name = Some("swift-otter".to_owned());
        if kind != "amp" {
            request.system_prompt_file = Some(base.clone().into());
            request.append_system_prompt_files = vec![fragment.clone().into()];
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
            accounts: &machine.accounts,
            bwrap: None,
            ambient_env: &BTreeMap::new(),
        })
        .unwrap();
        assert!(!root.exists(), "{kind} compilation must not write");
        assert!(plan.sandbox.is_none());
        let scratch = state.agents_dir.join("swift-otter");
        assert_eq!(
            plan.process().env.get(ENV_SCRATCH).map(PathBuf::from),
            Some(scratch.clone())
        );
        assert_eq!(
            plan.process().env.get(ENV_SHARED).map(PathBuf::from),
            Some(state.shared_dir.clone())
        );
        let reminder = &plan.process().reminder;
        assert!(reminder.contains("<system_reminder>"));
        assert_eq!(
            plan.reminder_channel.is_some(),
            matches!(kind, "claude" | "qwen" | "pi")
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
        assert!(
            scratch.is_dir(),
            "{kind}: host launches create their scratch dir"
        );
    }
}

#[test]
fn git_reminder_compile_uses_launch_cwd_and_effective_switch_for_children_too() {
    let project = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let machine = crate::config::MachineConfig::default();
    let mut effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    for subagent in [false, true] {
        for enabled in [false, true] {
            effective.git_reminder = enabled;
            for cwd in [project.path(), repo.path()] {
                let mut request =
                    ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
                request.subagent = subagent;
                let plan = compile(LaunchPlanInputs {
                    request: &request,
                    cwd,
                    project_root: project.path(),
                    rimz_bin: Path::new("/bin/rimz"),
                    runtime: &runtime,
                    state: &state,
                    effective: Some(&effective),
                    commands: &machine.agents.commands,
                    accounts: &machine.accounts,
                    bwrap: None,
                    ambient_env: &BTreeMap::new(),
                })
                .unwrap();
                assert_eq!(
                    plan.process().reminder.contains("$ git status --short"),
                    enabled && cwd == repo.path()
                );
                assert!(plan.warnings.is_empty());
                assert!(!repo.path().join(".git/index.lock").exists());
            }
        }
    }
}

/// A room whose `workspace.json` freezes `logins`, with the machine config
/// that declares those accounts.
fn room_with_accounts(
    root: &Path,
    accounts_toml: &str,
    logins: &[(&str, &str)],
) -> (crate::config::MachineConfig, StatePaths) {
    let machine = crate::config::MachineConfig {
        accounts: toml::from_str(accounts_toml).expect("accounts config"),
        ..crate::config::MachineConfig::default()
    };
    let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).expect("workspace");
    let state = StatePaths::under(workspace.workspace_id.clone(), root).expect("state paths");
    state.ensure_dirs().expect("state dirs");
    let mut record = crate::workspace::record::WorkspaceRecord::from_resolved(&workspace);
    record.logins = Some(
        logins
            .iter()
            .map(|(kind, name)| {
                (
                    AgentKind::new_unchecked(*kind),
                    name.parse().expect("login name"),
                )
            })
            .collect(),
    );
    crate::workspace::record::write(&state, &record).expect("write record");
    (machine, state)
}

#[test]
fn the_room_account_sets_the_provider_home_on_the_launched_process() {
    let project = tempfile::tempdir().expect("project");
    let home = project.path().join("claude-work");
    let (machine, state) = room_with_accounts(
        project.path(),
        &format!("[claude.work]\nhome = {:?}", home.display().to_string()),
        &[("claude", "work")],
    );
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime paths");
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());

    let plan = compile(LaunchPlanInputs {
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        bwrap: None,
        ambient_env: &BTreeMap::new(),
    })
    .expect("compile");

    assert_eq!(plan.login.key().to_string(), "claude@work");
    assert_eq!(
        plan.process()
            .env
            .get("CLAUDE_CONFIG_DIR")
            .map(String::as_str),
        Some(home.to_string_lossy().as_ref())
    );
    assert_eq!(plan.process().env.get("CODEX_HOME"), None);
}

#[test]
fn the_default_account_leaves_the_provider_home_to_the_provider() {
    let project = tempfile::tempdir().expect("project");
    let machine = crate::config::MachineConfig::default();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).expect("runtime");
    let state = StatePaths::under(workspace_id, project.path()).expect("state");

    for kind in ["claude", "codex"] {
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            bwrap: None,
            ambient_env: &BTreeMap::new(),
        })
        .expect("compile");

        assert!(plan.login.is_default(), "{kind}");
        assert_eq!(plan.process().env.get("CLAUDE_CONFIG_DIR"), None);
        assert_eq!(plan.process().env.get("CODEX_HOME"), None);
        assert_eq!(
            plan.process().env.get(Isolation::ENV).map(String::as_str),
            Some("host"),
            "{kind}"
        );
    }
}

#[test]
fn a_room_account_the_config_no_longer_declares_fails_the_launch() {
    let project = tempfile::tempdir().expect("project");
    let (machine, state) = room_with_accounts(project.path(), "", &[("claude", "work")]);
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime");
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());

    let error = compile(LaunchPlanInputs {
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        bwrap: None,
        ambient_env: &BTreeMap::new(),
    });

    let error = match error {
        Err(error) => error,
        Ok(_) => panic!("an undeclared account cannot launch"),
    };
    assert!(
        error.to_string().contains("rimz accounts add claude work"),
        "{error}"
    );
}

#[test]
fn the_sandbox_binds_and_pins_the_room_account_home() {
    let project = tempfile::tempdir().expect("project");
    let home = project.path().join("codex-personal");
    std::fs::create_dir_all(&home).expect("account home");
    let (machine, state) = room_with_accounts(
        project.path(),
        &format!("[codex.personal]\nhome = {:?}", home.display().to_string()),
        &[("codex", "personal")],
    );
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime paths");
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());

    let plan = compile(LaunchPlanInputs {
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        bwrap: Some(Path::new("/usr/bin/bwrap")),
        ambient_env: &BTreeMap::from([("HOME".to_owned(), project.path().display().to_string())]),
    })
    .expect("compile");

    assert!(
        plan.process()
            .argv
            .windows(2)
            .any(|pair| pair == ["--sandbox", "danger-full-access"])
    );
    assert_eq!(
        plan.process().env.get(Isolation::ENV).map(String::as_str),
        Some("sandbox")
    );
    assert_eq!(
        plan.process().env.get(ENV_SCRATCH).map(String::as_str),
        Some("/tmp/scratchpad")
    );
    assert_eq!(
        plan.process().env.get(ENV_SHARED).map(String::as_str),
        Some("/tmp/shared")
    );
    let sandbox = plan.sandbox.as_ref().expect("sandbox plan");
    let tmp_bind = sandbox
        .plan
        .mounts
        .iter()
        .position(|mount| matches!(mount, crate::sandbox::Mount::Bind { target, .. } if target == Path::new("/tmp")))
        .expect("room tmp bind");
    assert_eq!(
        sandbox.plan.mounts.get(tmp_bind + 1),
        Some(&crate::sandbox::Mount::Bind {
            source: state.scratchpad_dir.clone(),
            target: "/tmp/scratchpad".into(),
        }),
        "a launch without a handle binds the shared scratchpad over itself"
    );
    assert!(
        sandbox.plan.mounts.iter().any(
            |mount| matches!(mount, crate::sandbox::Mount::Bind { source, .. } if source == &home)
        ),
        "the named account home is bind-mounted"
    );
    assert_eq!(
        sandbox.pins.get("CODEX_HOME"),
        Some(&crate::sandbox::EnvPin::Set(
            home.to_string_lossy().into_owned()
        ))
    );
}
