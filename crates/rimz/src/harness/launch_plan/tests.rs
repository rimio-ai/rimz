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
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        loop_reminder: None,
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

fn recorded_session(model: &str) -> crate::agents::AgentState {
    let mut agent = crate::testkit::agent_state("codex", "session", jiff::Timestamp::now());
    agent.model = Some(model.into());
    agent
}

#[test]
fn recorded_alias_resume_ignores_observed_session_model() {
    check_alias_resume(false);
}

#[test]
fn explicit_alias_resume_ignores_legacy_observed_model() {
    check_alias_resume(true);
}

fn check_alias_resume(explicit: bool) {
    let root = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(root.path()),
        root.path(),
    )
    .unwrap();
    let machine = toml::from_str("[models.codex]\nsol = 'gpt-6-sol'").unwrap();
    let mut req = request(
        "codex",
        action_with_args("resume", vec!["--model=sol".into()]),
    );
    req.identity.params.model = Some("sol".into());
    req.identity.params.record = Some(Box::new(crate::agents::LaunchRecord {
        model: Some("sol".into()),
        ..Default::default()
    }));
    req.identity.resume_model_override = explicit;
    let session = crate::agents::AgentState {
        record: req.identity.params.record.clone().filter(|_| !explicit),
        ..recorded_session("observed-switch")
    };
    resolve_model(
        &mut req,
        &machine,
        &runtime,
        &ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
        Some(&session),
        None,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(req.identity.params.model.as_deref(), Some("gpt-6-sol"));
    assert_eq!(
        req.identity.params.record.unwrap().model.as_deref(),
        Some("sol")
    );
}

#[test]
fn model_alias_resolution_at_the_process_boundary() {
    use crate::agents::capabilities::{ModelCatalogEntry, ModelCatalogErr, ModelCatalogSource};
    struct Catalog(usize, bool);
    impl ModelCatalogSource for Catalog {
        fn fetch(
            &mut self,
            _: &RuntimePaths,
            _: &BTreeMap<String, String>,
        ) -> Result<Vec<ModelCatalogEntry>, ModelCatalogErr> {
            self.0 += 1;
            if self.1 {
                return Err(ModelCatalogErr::Unavailable("offline".into()));
            }
            Ok(vec![ModelCatalogEntry {
                id: "gpt-6.1-sol".into(),
                hidden: false,
                upgrade: None,
                efforts: vec!["high".into()],
            }])
        }
    }
    let root = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(root.path());
    let runtime = RuntimePaths::under(id.clone(), root.path()).unwrap();
    let state = StatePaths::under(id, root.path()).unwrap();
    let login = ProviderLogin::default_for(AgentKind::new_unchecked("codex"));
    let machine = crate::config::MachineConfig::default();
    let ambient = BTreeMap::new();
    let mut catalog = Catalog(0, false);
    for (action, requested, recorded, expected) in [
        ("launch", "sol", None, "gpt-6.1-sol"),
        ("resume", "sol", Some("gpt-6-sol"), "gpt-6-sol"),
        ("fork", "sol", Some("gpt-6-sol"), "gpt-6-sol"),
        ("resume", "gpt-6-sol", Some("gpt-6.1-sol"), "gpt-6-sol"),
        ("resume", "sol", None, "gpt-6.1-sol"),
        ("resume", "sol", Some("sol"), "gpt-6.1-sol"),
    ] {
        let mut req = request(
            "codex",
            action_with_args(action, vec![format!("--model={requested}")]),
        );
        req.identity.params.model = Some(requested.into());
        let (warnings, movement) = resolve_model(
            &mut req,
            &machine,
            &runtime,
            &login,
            recorded.map(recorded_session).as_ref(),
            Some(&mut catalog),
            &ambient,
        )
        .unwrap();
        assert_eq!(req.identity.params.model.as_deref(), Some(expected));
        if requested == "sol" {
            assert_eq!(req.action.extra_args(), ["--model", expected]);
        }
        if action == "launch" {
            assert_eq!(movement.unwrap().to, expected);
        }
        if action == "resume" && requested == "sol" && recorded.is_none() {
            assert!(!warnings.is_empty());
        }
        let plan = compile(LaunchPlanInputs {
            request: &req,
            cwd: root.path(),
            project_root: root.path(),
            rimz_bin: Path::new("rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
        })
        .unwrap();
        assert_eq!(
            plan.process()
                .env
                .get("RIMZ_AGENT_MODEL")
                .map(String::as_str),
            Some(expected)
        );
    }
    assert_eq!(catalog.0, 1);
    let pinned: crate::config::MachineConfig =
        toml::from_str("[models.codex]\nsol = 'gpt-6-sol'").unwrap();
    let mut req = request(
        "codex",
        action_with_args("launch", vec!["--model".into(), "sol".into()]),
    );
    req.identity.params.model = Some("sol".into());
    let (warnings, movement) = resolve_model(
        &mut req,
        &pinned,
        &runtime,
        &login,
        None,
        Some(&mut catalog),
        &ambient,
    )
    .unwrap();
    assert_eq!(req.identity.params.model.as_deref(), Some("gpt-6-sol"));
    assert!(!warnings.is_empty());
    assert!(movement.is_none());
    assert_eq!(catalog.0, 1);
    let cache_path = runtime.shared_model_catalog_path(&login.key());
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    cache["fetched_at"] = 0.into();
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
    catalog.1 = true;
    for (requested, expected, calls, warned) in [
        ("gpt-6-sol", "gpt-6-sol", 1, false),
        ("sol", "gpt-6.1-sol", 2, true),
        ("sol", "gpt-6-sol", 3, true),
    ] {
        req.identity.params.model = Some(requested.into());
        *req.action.extra_args_mut() = vec!["--model".into(), requested.into()];
        let (warnings, movement) = resolve_model(
            &mut req,
            &machine,
            &runtime,
            &login,
            None,
            Some(&mut catalog),
            &ambient,
        )
        .unwrap();
        assert_eq!(req.identity.params.model.as_deref(), Some(expected));
        assert_eq!(req.action.extra_args(), ["--model", expected]);
        assert_eq!(!warnings.is_empty(), warned);
        assert!(movement.is_none());
        assert_eq!(catalog.0, calls);
        if calls == 2 {
            std::fs::remove_file(&cache_path).unwrap();
        }
    }
}

#[test]
fn routine_permissions_cover_actions_children_and_isolations() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let home = project.path().join("home");
    for name in ["commit", "rimz-pin"] {
        std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
        std::fs::write(
            home.join("skills").join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\nSkill."),
        )
        .unwrap();
    }
    let ambient = BTreeMap::from([
        ("CLAUDE_CONFIG_DIR".into(), home.display().to_string()),
        (
            "RIMZ_AGENTS_HOME".into(),
            project.path().join("agents").display().to_string(),
        ),
        ("HOME".into(), home.display().to_string()),
    ]);
    for (enabled, listed, rules) in [
        (false, false, false),
        (false, false, true),
        (false, true, false),
        (false, true, true),
        (true, false, false),
        (true, false, true),
        (true, true, false),
        (true, true, true),
    ] {
        let machine: crate::config::MachineConfig =
            toml::from_str(&format!("[agents]\nallow-routine-rimz = {enabled}")).unwrap();
        let effective =
            crate::config::effective::load_with_roots(&machine, project.path(), config.path())
                .unwrap();
        for action in ["launch", "resume", "fork"] {
            for sandboxed in [false, true] {
                for subagent in [false, true] {
                    let mut request = request("claude", action_with_args(action, Vec::new()));
                    request.subagent = subagent;
                    request.identity.name = Some("otter".into());
                    request.skills = listed.then(|| vec!["commit".parse().unwrap()]);
                    request.allowed_tools = rules.then(|| vec!["Bash(git *)".parse().unwrap()]);
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
                        bwrap: sandboxed.then_some(Path::new("/bin/bwrap")),
                        agents: Ok(&[]),
                        ambient_env: &ambient,
                        agent_shell: None,
                    })
                    .unwrap();
                    let settings = plan
                        .process()
                        .provider_argv
                        .windows(2)
                        .find(|pair| pair[0] == "--settings");
                    assert_eq!(
                        settings.is_some(),
                        enabled || listed || rules,
                        "{action} sandbox={sandboxed} child={subagent}"
                    );
                    if let Some(settings) = settings {
                        let value: serde_json::Value = serde_json::from_str(&settings[1]).unwrap();
                        assert_eq!(
                            value["permissions"]["allow"].as_array().is_some_and(
                                |allow| allow.contains(&serde_json::json!("Bash(git *)"))
                            ),
                            rules
                        );
                        let dirs = [
                            if sandboxed {
                                PathBuf::from("/tmp")
                            } else {
                                state.temp_unit_dir(Some("otter"))
                            },
                            state.room_shared_dir.clone(),
                        ];
                        assert_eq!(
                            value["permissions"]["additionalDirectories"],
                            if enabled {
                                serde_json::json!(dirs)
                            } else {
                                serde_json::Value::Null
                            }
                        );
                        assert_eq!(
                            value["permissions"]["allow"]
                                .as_array()
                                .is_some_and(|allow| allow
                                    .contains(&serde_json::json!("Bash(rimz agents *)"))),
                            enabled
                        );
                        assert_eq!(
                            value["permissions"]["allow"].as_array().is_some_and(
                                |allow| allow.contains(&serde_json::json!("Skill(rimz-pin)"))
                            ),
                            enabled,
                            "{action} sandbox={sandboxed} child={subagent}"
                        );
                        assert_eq!(
                            value["permissions"]["allow"].as_array().is_some_and(
                                |allow| allow.contains(&serde_json::json!("Skill(commit)"))
                            ),
                            listed
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn sandbox_settings_artifact_is_written_only_on_apply() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    std::fs::write(
        project.path().join("profile.json"),
        r#"{"env":{"TOKEN":"private-secret"}}"#,
    )
    .unwrap();
    let request = request(
        "claude",
        action_with_args("launch", vec!["--settings".into(), "profile.json".into()]),
    );
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
        bwrap: Some(Path::new("/bin/bwrap")),
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell: None,
    })
    .unwrap();
    let args = &plan.process().provider_argv;
    let settings = args
        .windows(2)
        .find(|pair| pair[0] == "--settings")
        .unwrap();
    let path = Path::new(&settings[1]);
    assert_eq!(path.parent(), Some(runtime.prompt_dir().as_path()));
    assert!(!path.exists());
    assert!(!args.join(" ").contains("private-secret"));
    apply(&plan).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(value["env"]["TOKEN"], "private-secret");
    assert!(value["permissions"]["allow"].is_array());
}

#[test]
fn allowed_tools_warn_on_unsupported_kind_without_changing_argv() {
    let project = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let machine = crate::config::MachineConfig::default();
    let ambient = BTreeMap::from([("HOME".into(), project.path().display().to_string())]);
    let compile_request = |request: &ExecRequest| {
        compile(LaunchPlanInputs {
            request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
        })
        .unwrap()
    };
    let mut request = request("codex", action_with_args("launch", Vec::new()));
    let baseline = compile_request(&request);
    for rules in [Some(vec!["Read".parse().unwrap()]), Some(Vec::new()), None] {
        request.allowed_tools = rules;
        let plan = compile_request(&request);
        assert_eq!(
            plan.process().provider_argv,
            baseline.process().provider_argv
        );
        let warnings: Vec<_> = plan
            .warnings
            .iter()
            .map(ToString::to_string)
            .filter(|text| text.contains("allowed-tools"))
            .collect();
        if request
            .allowed_tools
            .as_ref()
            .is_some_and(|rules| !rules.is_empty())
        {
            assert_eq!(
                warnings,
                [
                    "codex has no per-launch permission rules; allowed-tools is not applied and the agent prompts as usual: remove allowed-tools from the definition or run it on claude"
                ]
            );
        } else {
            assert!(warnings.is_empty());
        }
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
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell: None,
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
fn shared_lsp_configuration_reaches_child_reminders_without_a_live_server() {
    let machine: crate::config::MachineConfig = toml::from_str("[lsp.servers.rust]\ncommand = ['rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']").unwrap();
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), config.path()).unwrap();
    for subagent in [false, true] {
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        request.subagent = subagent;
        let (reminder, _) = reminders(&request, Some(&effective), &machine.agents.commands);
        assert!(reminder.lsp_configured);
        assert!(reminder.lsp_servers.is_empty());
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
    let mut machine_config = crate::config::MachineConfig::default();
    for (name, agent, model_reminder) in [("quiet", "claude", false), ("loud", "quiet", true)] {
        machine_config.agents.profiles.0.insert(
            name.to_owned(),
            crate::config::Profile {
                definition_renders: None,
                model_tier: None,
                tier_stamp: None,
                agent: agent.to_owned(),
                isolation: None,
                model_reminder: Some(model_reminder),
                keep_warm: None,
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
                allowed_tools: None,
                args: None,
            },
        );
    }
    for (name, model_reminder) in [("quiet", true), ("child", false)] {
        machine_config.subagents.profiles.0.insert(
            name.to_owned(),
            crate::config::Profile {
                definition_renders: None,
                model_tier: None,
                tier_stamp: None,
                agent: "claude".to_owned(),
                isolation: None,
                model_reminder: Some(model_reminder),
                keep_warm: None,
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
                allowed_tools: None,
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
                    idle_compact: None,
                    keep_warm: None,
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
                    idle_compact: None,
                    keep_warm: None,
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
    // The effective team supplies the member identities, not model names.
    let context = crate::harness::launch_context::team_launch_context(
        &request.identity.params,
        &request.action,
        &team,
        project.path(),
    )
    .expect("team context");
    assert!(crate::harness::launch_context::reminder(&context).contains(
        "Resumed session; your earlier context continues.\n\nMembers: @planner, @coder (you)."
    ));
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
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
        })
        .unwrap();
        assert!(!root.exists(), "{kind} compilation must not write");
        assert!(plan.sandbox.is_none());
        assert!(!plan.process().env.contains_key("SCCACHE_CLIENT_SIDE"));
        let unit = state.temp_unit_dir(Some("swift-otter"));
        assert_eq!(
            plan.process().env.get("TMPDIR").map(PathBuf::from),
            Some(unit.clone())
        );
        assert_eq!(
            plan.process().env.get("RIMZ_SHARED").map(PathBuf::from),
            Some(state.room_shared_dir.clone())
        );
        assert!(!plan.process().env.keys().any(|key| key.contains("SCRATCH")));
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
            unit.is_dir() && state.room_shared_dir.is_dir(),
            "{kind}: host launches create their temp unit and the shared dir"
        );
    }
}

#[test]
fn env_reminder_compile_resolves_worktree_from_launch_cwd() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let created = crate::worktree::create(
        &repo,
        &crate::config::WorktreeConfig {
            dir: root.join("worktrees").display().to_string(),
            ..Default::default()
        },
        Some("demo"),
        None,
        None,
        false,
    )
    .unwrap();
    let cwd = &created.marker.worktree_path;
    let machine = crate::config::MachineConfig::default();
    let mut effective = crate::config::effective::load_with_roots(&machine, &root, &root).unwrap();
    let id = crate::WorkspaceId::from_project_root(&root);
    let runtime = RuntimePaths::under(id.clone(), &root).unwrap();
    let state = StatePaths::under(id, &root).unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    for subagent in [false, true] {
        request.subagent = subagent;
        for enabled in [true, false] {
            effective.env_reminder = enabled;
            let plan = compile(LaunchPlanInputs {
                request: &request,
                cwd,
                project_root: &root,
                rimz_bin: Path::new("/bin/rimz"),
                runtime: &runtime,
                state: &state,
                effective: Some(&effective),
                commands: &machine.agents.commands,
                accounts: &machine.accounts,
                bwrap: None,
                agents: Ok(&[]),
                ambient_env: &BTreeMap::new(),
                agent_shell: None,
            })
            .unwrap();
            let reminder = &plan.process().reminder;
            assert_eq!(
                reminder.contains(&format!(
                    "- cwd: {}\n- worktree: branched from main; primary checkout at {}",
                    cwd.display(),
                    repo.display()
                )),
                enabled
            );
            assert_eq!(reminder.contains("- worktree:"), enabled);
            assert!(plan.warnings.is_empty());
        }
    }
}

#[test]
fn env_reminder_compile_uses_launch_cwd_and_effective_switch_for_children_too() {
    let project = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let mut effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    for subagent in [false, true] {
        for enabled in [false, true] {
            effective.env_reminder = enabled;
            for cwd in [project.path(), other.path()] {
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
                    agents: Ok(&[]),
                    ambient_env: &BTreeMap::new(),
                    agent_shell: None,
                })
                .unwrap();
                assert_eq!(
                    plan.process()
                        .reminder
                        .contains(&format!("### Environment\n\n- cwd: {}", cwd.display())),
                    enabled
                );
                assert!(!plan.process().reminder.contains("git status"));
                assert!(plan.warnings.is_empty());
            }
        }
    }
}

#[test]
fn runtime_env_compile_stamps_the_switch_and_moves_the_listing_out_of_the_reminder() {
    use crate::harness::launch::ENV_RUNTIME_ENV;

    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let mut effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    effective.teams.0.insert(
        "forge".to_owned(),
        toml::from_str(
            "leader = 'planner'\n[[roles]]\nrole = 'planner'\nprofile = 'claude'\nowns = ['Plan']",
        )
        .unwrap(),
    );
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    #[derive(Clone, Copy, Debug)]
    enum Hooks {
        Absent,
        Untrusted,
        Wired,
    }
    for (kind, flag, configured, hooks, runtime_env) in [
        ("claude", true, true, Hooks::Wired, true),
        ("codex", true, true, Hooks::Wired, true),
        ("claude", true, false, Hooks::Wired, true),
        ("claude", false, true, Hooks::Wired, false),
        ("droid", true, true, Hooks::Wired, false),
        ("claude", true, true, Hooks::Absent, false),
        ("codex", true, true, Hooks::Absent, false),
        ("codex", true, true, Hooks::Untrusted, false),
    ] {
        let home = tempfile::tempdir().unwrap();
        let ambient = BTreeMap::from(
            [
                (ENV_RUNTIME_ENV, "1".to_owned()),
                ("HOME", home.path().display().to_string()),
                ("CLAUDE_CONFIG_DIR", home.path().display().to_string()),
                ("CODEX_HOME", home.path().display().to_string()),
            ]
            .map(|(key, value)| (key.to_owned(), value)),
        );
        let adapter = crate::agents::find_definition(kind).unwrap();
        if !matches!(hooks, Hooks::Absent) {
            adapter.install_hooks(&ambient).unwrap();
        }
        if matches!(hooks, Hooks::Wired) && kind == "codex" {
            // Codex keys a trusted hook by config path and lower_snake event.
            let config = home.path().join("config.toml");
            let mut text = std::fs::read_to_string(&config).unwrap();
            for event in adapter
                .managed_integration()
                .unwrap()
                .untrusted_preflight_hooks(&ambient)
            {
                let token = event.chars().fold(String::new(), |mut token, c| {
                    if c.is_ascii_uppercase() && !token.is_empty() {
                        token.push('_');
                    }
                    token.push(c.to_ascii_lowercase());
                    token
                });
                text.push_str(&format!(
                    "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
                    config.display()
                ));
            }
            std::fs::write(&config, text).unwrap();
        }
        effective.runtime_env = flag;
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        request.identity.params.team = Some("forge".to_owned());
        request.identity.params.role = Some("planner".to_owned());
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: configured.then_some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
        })
        .unwrap();
        let process = plan.process();
        let case = format!("{kind} flag={flag} configured={configured} hooks={hooks:?}");
        assert_eq!(
            process.env.get(ENV_RUNTIME_ENV).map(String::as_str),
            runtime_env.then_some("1"),
            "{case}"
        );
        assert_eq!(
            process.unset.contains(ENV_RUNTIME_ENV),
            !runtime_env,
            "{case}"
        );
        assert_eq!(
            process.reminder.contains("$ ls blackboard.md *-notes.md"),
            configured && !runtime_env,
            "{case}: the listing is in the reminder or at prompt submit, never both"
        );
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
fn the_stamped_account_sets_the_provider_home_after_the_room_switches() {
    let project = tempfile::tempdir().expect("project");
    let home = project.path().join("claude-work");
    let (machine, state) = room_with_accounts(
        project.path(),
        &format!(
            "[claude.work]\nhome = {:?}\n[claude.personal]\nhome = {:?}",
            home.display().to_string(),
            project.path().join("personal").display().to_string()
        ),
        &[("claude", "personal")],
    );
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime paths");
    for action in ["launch", "resume", "fork"] {
        let mut request = request("claude", action_with_args(action, Vec::new()));
        request.identity.params.login = Some("work".parse().unwrap());

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
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
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
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
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
fn a_stamped_account_the_config_no_longer_declares_fails_the_launch() {
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
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.login = Some("work".parse().unwrap());

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
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell: None,
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
fn the_sandbox_binds_and_pins_the_stamped_account_home() {
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
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    request.identity.params.login = Some("personal".parse().unwrap());

    for (cwd, expected) in [
        (project.path().to_path_buf(), project.path().to_path_buf()),
        (
            state.temp_unit_dir(None).join("clean"),
            PathBuf::from("/tmp/clean"),
        ),
    ] {
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: &cwd,
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            bwrap: Some(Path::new("/usr/bin/bwrap")),
            agents: Ok(&[]),
            ambient_env: &BTreeMap::from([(
                "HOME".to_owned(),
                project.path().display().to_string(),
            )]),
            agent_shell: None,
        })
        .expect("compile cwd");
        assert_eq!(plan.cwd, cwd);
        assert!(
            plan.process()
                .argv
                .windows(2)
                .any(|pair| pair[0] == "--chdir" && Path::new(&pair[1]) == expected)
        );
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
        bwrap: Some(Path::new("/usr/bin/bwrap")),
        agents: Ok(&[]),
        ambient_env: &BTreeMap::from([
            ("HOME".to_owned(), project.path().display().to_string()),
            ("SCCACHE_CLIENT_SIDE".to_owned(), "0".to_owned()),
            (
                crate::child_process::TEMP_ROOT_KEYS_ENV.to_owned(),
                "CLAUDE_CODE_TMPDIR".to_owned(),
            ),
        ]),
        agent_shell: None,
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
        plan.process().env.get("TMPDIR").map(String::as_str),
        Some("/tmp")
    );
    assert!(
        !plan
            .process()
            .env
            .contains_key(crate::child_process::USER_TMPDIR_ENV),
        "a mux child inside the view keeps /tmp"
    );
    assert!(
        plan.process().unset.contains("CLAUDE_CODE_TMPDIR"),
        "a parent's temp root stays unset after the sandbox pins"
    );
    assert_eq!(
        plan.process().env.get("RIMZ_SHARED").map(PathBuf::from),
        Some(state.room_shared_dir.clone())
    );
    assert_eq!(
        plan.process()
            .env
            .get("SCCACHE_CLIENT_SIDE")
            .map(String::as_str),
        Some("1"),
        "the view compiles client-side over an ambient opt-out"
    );
    let sandbox = plan.sandbox.as_ref().expect("sandbox plan");
    assert!(
        sandbox.plan.mounts.contains(&crate::sandbox::Mount::Bind {
            source: state.temp_unit_dir(None),
            target: "/tmp".into(),
        }),
        "a launch without a handle binds the unnamed unit"
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

#[test]
fn a_launch_saves_the_user_tmpdir_it_replaces() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    let other_unit = state.temp_unit_dir(Some("otter")).display().to_string();
    let saved = |ambient: &[(&str, &str)]| {
        let ambient = ambient
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
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
            agent_shell: None,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
        })
        .unwrap();
        assert_eq!(
            plan.process().env.get("TMPDIR").map(PathBuf::from),
            Some(state.temp_unit_dir(None))
        );
        plan.process()
            .env
            .get(crate::child_process::USER_TMPDIR_ENV)
            .cloned()
    };
    let user = crate::child_process::USER_TMPDIR_ENV;

    assert_eq!(
        saved(&[("TMPDIR", "/var/folders/t")]).as_deref(),
        Some("/var/folders/t")
    );
    assert_eq!(saved(&[]).as_deref(), Some(""), "present and empty is none");
    assert_eq!(saved(&[("TMPDIR", "")]).as_deref(), Some(""));
    assert_eq!(
        saved(&[(user, "/var/folders/t"), ("TMPDIR", &other_unit)]).as_deref(),
        Some("/var/folders/t"),
        "a launch inside an agent's tree carries the user's value forward"
    );
    assert_eq!(
        saved(&[(user, ""), ("TMPDIR", &other_unit)]).as_deref(),
        Some("")
    );
}

#[test]
fn a_provider_temp_root_follows_tmpdir() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let keys = crate::child_process::TEMP_ROOT_KEYS_ENV;
    for (kind, ambient, listed, unset) in [
        ("claude", None, "CLAUDE_CODE_TMPDIR", &[][..]),
        ("codex", None, "", &[]),
        (
            "codex",
            Some("CLAUDE_CODE_TMPDIR"),
            "",
            &["CLAUDE_CODE_TMPDIR"],
        ),
        (
            "claude",
            Some("CLAUDE_CODE_TMPDIR X_TMPDIR"),
            "CLAUDE_CODE_TMPDIR",
            &["X_TMPDIR"],
        ),
    ] {
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        let ambient = ambient
            .map(|list| BTreeMap::from([(keys.to_owned(), list.to_owned())]))
            .unwrap_or_default();
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
            agent_shell: None,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
        })
        .unwrap();
        let env = &plan.process().env;
        let expected = (kind == "claude").then(|| env["TMPDIR"].clone());
        assert_eq!(env.get("CLAUDE_CODE_TMPDIR").cloned(), expected, "{kind}");
        assert_eq!(
            env.get(keys).map(String::as_str),
            Some(listed),
            "{kind} over {ambient:?}"
        );
        assert_eq!(
            plan.process()
                .unset
                .iter()
                .map(String::as_str)
                // The runtime switch is cleared or stamped by its own rule, which is not this test's subject.
                .filter(|key| *key != crate::harness::launch::ENV_RUNTIME_ENV)
                .collect::<Vec<_>>(),
            unset,
            "{kind} over {ambient:?}"
        );
    }
}

#[test]
fn a_launched_child_shares_its_parent_unit_across_restart() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let mut parent =
        crate::testkit::agent_state("claude", "parent-session", jiff::Timestamp::UNIX_EPOCH);
    parent.name = Some("otter".to_owned());
    // A restart keeps the durable parent stamp and drops `subagent`.
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.name = Some("fox".to_owned());
    request.identity.params.parent_agent_id = Some("parent-session".into());
    request.identity.params.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
    let ambient = BTreeMap::from([("XDG_RUNTIME_DIR".to_owned(), "/run/user/1000".to_owned())]);
    let compile_with = |agents: Result<&[crate::agents::AgentState], &str>| {
        compile(LaunchPlanInputs {
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            agent_shell: None,
            bwrap: None,
            agents,
            ambient_env: &ambient,
        })
        .unwrap()
    };
    let parented = |plan: &LaunchPlan| {
        plan.warnings
            .iter()
            .any(|warning| warning.to_string().contains("parent-session"))
    };

    let plan = compile_with(Ok(std::slice::from_ref(&parent)));
    assert_eq!(
        plan.process().env.get("TMPDIR").map(PathBuf::from),
        Some(state.temp_unit_dir(Some("otter")))
    );
    assert!(
        plan.process()
            .reminder
            .contains("every temporary file you make. You share it with your caller;")
    );
    assert!(!parented(&plan));
    if cfg!(target_os = "linux") {
        assert_eq!(
            plan.process()
                .env
                .get("ZELLIJ_SOCKET_DIR")
                .map(String::as_str),
            Some("/run/user/1000/zellij"),
            "moving TMPDIR pins the socket base the wrapper resolved"
        );
    }

    let orphan = compile_with(Ok(&[]));
    assert_eq!(
        orphan.process().env.get("TMPDIR").map(PathBuf::from),
        Some(state.temp_unit_dir(Some("fox")))
    );
    assert!(
        orphan
            .process()
            .reminder
            .contains("every temporary file you make. You share it with your caller;"),
        "a child is never told it has subagents"
    );
    assert!(parented(&orphan), "a lost parent row warns");
    let unread = compile_with(Err("snapshot unreadable"));
    let warning = unread
        .warnings
        .iter()
        .map(ToString::to_string)
        .find(|warning| warning.contains("parent-session"))
        .expect("an unread room warns");
    assert!(
        warning.contains("snapshot unreadable") && !warning.contains("has no row"),
        "{warning}"
    );
    assert_eq!(
        unread.process().env.get("TMPDIR").map(PathBuf::from),
        Some(state.temp_unit_dir(Some("fox")))
    );

    let mut child =
        crate::testkit::agent_state("claude", "child-session", jiff::Timestamp::UNIX_EPOCH);
    child.name = Some("fox".to_owned());
    child.parent_agent_id = Some("parent-session".into());
    assert_eq!(
        agent_temp_unit(&child, std::slice::from_ref(&parent), &state),
        state.temp_unit_dir(Some("otter")),
        "agents show names the unit the launch used"
    );
}

#[test]
fn a_shared_codex_account_points_its_databases_at_the_default_home_without_touching_either() {
    let project = tempfile::tempdir().expect("project");
    let work = project.path().join("codex-work");
    let solo = project.path().join("codex-solo");
    let native = project.path().join("home/.codex");
    for dir in [&work, &solo, &native] {
        std::fs::create_dir_all(dir).expect("home");
    }
    std::fs::write(native.join("config.toml"), "").expect("default config");
    let (machine, state) = room_with_accounts(
        project.path(),
        &format!(
            "[codex.work]\nhome = {:?}\n[codex.solo]\nhome = {:?}\nhistory = \"standalone\"",
            work.display().to_string(),
            solo.display().to_string()
        ),
        &[("codex", "work")],
    );
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime paths");
    // The pane this launch starts from runs on the other account.
    let ambient = BTreeMap::from([
        (
            "HOME".to_owned(),
            project.path().join("home").display().to_string(),
        ),
        ("CODEX_HOME".to_owned(), solo.display().to_string()),
    ]);
    for (login, databases) in [("work", Some(&native)), ("solo", None)] {
        let mut request = request("codex", action_with_args("launch", Vec::new()));
        request.identity.params.login = Some(login.parse().unwrap());
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
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
        })
        .expect("compile");
        assert_eq!(
            plan.process()
                .env
                .get("CODEX_SQLITE_HOME")
                .map(PathBuf::from),
            databases.cloned(),
            "{login}"
        );
    }
    for home in [&work, &solo] {
        assert_eq!(std::fs::read_dir(home).expect("home").count(), 0);
    }
}

fn host_request(kind: &str, action: ExecAction) -> ExecRequest {
    let mut request = request(kind, action);
    request.identity.params.isolation = Some(Isolation::Host);
    request
}

fn envelope(request: &ExecRequest, prompt_file: Option<&Path>) -> launch::ExecEnvelope {
    let mut wire = serde_json::to_value(request).unwrap();
    if let Some(path) = prompt_file {
        wire["prompt_file"] = path.display().to_string().into();
    }
    launch::decode_exec_envelope(request.kind.as_str(), None, &wire.to_string()).unwrap()
}

fn prepare(
    request: &ExecRequest,
    prompt_file: Option<&Path>,
    machine: &crate::config::MachineConfig,
    effective: Option<&LaunchAgents>,
    room_agents: &dyn Fn() -> Result<Vec<crate::agents::AgentState>, String>,
) -> ExecPreparation {
    let root = tempfile::tempdir().unwrap();
    prepare_exec(
        envelope(request, prompt_file),
        root.path(),
        root.path(),
        Path::new("/bin/rimz"),
        machine,
        effective,
        room_agents,
        |_, _| Ok(Vec::new()),
    )
}

#[test]
fn exec_refuses_only_fresh_launches_of_an_unshadowed_failed_profile() {
    let root = tempfile::tempdir().unwrap();
    let mut machine = crate::config::MachineConfig::default();
    let path = PathBuf::from("/tmp/.agents/agents/worker.md");
    machine
        .notices
        .failed_definitions
        .insert("worker".to_owned(), [path.clone()].into());
    machine
        .notices
        .definition_errors
        .push(crate::config::definitions::DefinitionErr {
            path,
            message: "invalid frontmatter".to_owned(),
            cause: crate::config::definitions::DefinitionCause::Invalid,
        });
    let request = |action| {
        let mut request = request("codex", action);
        request.identity.params.profile = Some("worker".to_owned());
        request
    };
    let launch = request(ExecAction::Launch {
        prompt: None,
        extra_args: Vec::new(),
    });
    let resume = request(ExecAction::Resume {
        session_id: "s".to_owned(),
        extra_args: Vec::new(),
    });
    let fork = request(ExecAction::Fork {
        session_id: "s".to_owned(),
        extra_args: Vec::new(),
    });
    let mut effective =
        crate::config::effective::load_with_roots(&machine, root.path(), &root.path().join("home"))
            .unwrap();

    let detail = "/tmp/.agents/agents/worker.md: invalid frontmatter";
    assert_eq!(
        definition_failure(&launch, &machine, Some(&effective)).as_deref(),
        Some(detail)
    );
    let refused = prepare(&launch, None, &machine, Some(&effective), &|| {
        panic!("the definition gate reads no room")
    })
    .outcome
    .err()
    .expect("a failed definition refuses a fresh launch");
    assert_eq!(refused.to_string(), detail);
    assert!(std::error::Error::source(&refused).is_none());
    assert!(definition_failure(&launch, &machine, None).is_some());
    assert_eq!(
        definition_failure(&resume, &machine, Some(&effective)),
        None
    );
    assert_eq!(definition_failure(&fork, &machine, Some(&effective)), None);

    let shadow: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
    effective.profiles.0.insert("worker".to_owned(), shadow);
    assert_eq!(
        definition_failure(&launch, &machine, Some(&effective)),
        None
    );
}

#[test]
fn exec_preparation_reads_the_room_only_for_a_resume_or_a_child() {
    let machine = crate::config::MachineConfig::default();
    let reads = std::cell::Cell::new(0);
    let unreadable = || {
        reads.set(reads.get() + 1);
        Err("snapshot unreadable".to_owned())
    };

    let fresh = host_request("codex", action_with_args("launch", Vec::new()));
    let prepared = prepare(&fresh, None, &machine, None, &|| {
        panic!("a fresh launch reads no room")
    });
    let (plan, isolation) = prepared.outcome.expect("a fresh launch compiles");
    assert_eq!(isolation, Isolation::Host);
    assert!(
        plan.warnings
            .iter()
            .all(|warning| !matches!(warning, LaunchPlanWarning::ParentUnitUnread { .. }))
    );

    let resume = host_request("codex", action_with_args("resume", Vec::new()));
    let prepared = prepare(&resume, None, &machine, None, &unreadable);
    assert!(
        prepared.outcome.is_ok(),
        "an unread room never fails a resume"
    );
    assert_eq!(reads.get(), 1);

    let mut child = host_request("codex", action_with_args("launch", Vec::new()));
    child.identity.name = Some("fox".to_owned());
    child.identity.params.parent_agent_id = Some("parent-session".into());
    let prepared = prepare(&child, None, &machine, None, &unreadable);
    let (plan, _) = prepared.outcome.expect("a child compiles without its room");
    assert_eq!(reads.get(), 2);
    assert!(plan.warnings.iter().any(|warning| matches!(
        warning,
        LaunchPlanWarning::ParentUnitUnread { error, .. } if error == "snapshot unreadable"
    )));
}

#[test]
fn exec_preparation_names_a_missing_launch_prompt() {
    let missing = tempfile::tempdir().unwrap().path().join("task.md");
    let launch = host_request("codex", action_with_args("launch", Vec::new()));
    let err = prepare(
        &launch,
        Some(&missing),
        &crate::config::MachineConfig::default(),
        None,
        &|| panic!("a fresh launch reads no room"),
    )
    .outcome
    .err()
    .expect("a missing prompt artifact fails the launch");
    assert_eq!(err.to_string(), "materializing launch prompt");
    assert!(matches!(
        std::error::Error::source(&err)
            .and_then(|source| source.downcast_ref::<launch::ExecWireErr>()),
        Some(launch::ExecWireErr::PromptRead { path, .. }) if *path == missing
    ));
}

#[test]
fn exec_preparation_returns_model_warnings_when_compile_fails() {
    let machine: crate::config::MachineConfig =
        toml::from_str("[models.claude]\nsol = 'claude-sol-1'").unwrap();
    let mut resume = host_request("claude", action_with_args("resume", Vec::new()));
    resume.identity.params.model = Some("sol".into());
    resume.system_prompt_file = Some(crate::config::PromptSource::File(
        tempfile::tempdir().unwrap().path().join("missing.md"),
    ));
    let prepared = prepare(&resume, None, &machine, None, &|| Ok(Vec::new()));
    assert!(
        prepared.outcome.is_err(),
        "the missing prompt fails compile"
    );
    assert!(
        prepared
            .model_warnings
            .iter()
            .any(|warning| warning.contains("has no recorded session model")),
        "{:?}",
        prepared.model_warnings
    );
}
