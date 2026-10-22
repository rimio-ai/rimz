use super::*;

#[test]
fn compiler_refuses_only_the_kind_with_an_unsupported_budget() {
    if std::env::var_os("RIMZ_TEST_COMPILE_BUDGET_HOME").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "harness::launch::tests::compiler_refuses_only_the_kind_with_an_unsupported_budget",
                "--nocapture",
            ])
            .env("RIMZ_HOME", home.path().join(".rimz"))
            .env("RIMZ_TEST_COMPILE_BUDGET_HOME", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let path = crate::config::MachineConfig::config_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "[accounts.budget]\ncopilot = \"10/day\"\n").unwrap();
    let root = tempfile::tempdir().unwrap();
    for kind in ["copilot", "codex"] {
        let result = compile_agent_process_with_extra_env(
            None,
            root.path(),
            &request(
                kind,
                ExecAction::Launch {
                    prompt: None,
                    extra_args: Vec::new(),
                },
            ),
            root.path(),
            &BTreeMap::new(),
            &LaunchReminders::default(),
        );
        if kind == "copilot" {
            let Err(error) = result else {
                panic!("unsupported budget must refuse");
            };
            assert!(
                error
                    .to_string()
                    .contains("unsupported `accounts.budget.copilot`")
            );
        } else {
            assert!(result.is_ok(), "{}", result.err().unwrap());
        }
    }
}

#[test]
fn appended_system_file_and_text_share_one_slot() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("rules.txt"), "keep the user's rules").unwrap();
    let channel = SystemTextChannel::TextFlag {
        flags: vec![
            "--append-system-prompt".into(),
            "--append-system-prompt-file".into(),
        ],
    };
    let files = [crate::agents::PresetArgMatcher::Flag(vec![
        "--append-system-prompt-file".into(),
    ])];
    for args in [
        vec!["--append-system-prompt-file", "rules.txt"],
        vec!["--append-system-prompt-file=rules.txt"],
    ] {
        let mut args = args.into_iter().map(str::to_owned).collect();
        merge_appended_system_text(&mut args, &channel, "RimZ reminder", &files, root.path())
            .unwrap();
        assert_eq!(
            args,
            [
                "--append-system-prompt",
                "keep the user's rules\n\nRimZ reminder"
            ],
            "the file and inline form must not conflict at launch"
        );
    }
    let mut args = vec!["--append-system-prompt-file".into(), "missing.txt".into()];
    assert!(
        merge_appended_system_text(&mut args, &channel, "reminder", &files, root.path()).is_err(),
        "an unreadable configured prompt must fail at launch"
    );
}
use crate::harness::launch_reminders::{TempFiles, subagent_reminder, wrap};

#[test]
fn muse_managed_hook_passthrough_covers_host_and_sandbox_launch_environment() {
    let adapter = crate::agents::find_definition("muse");
    assert!(adapter.is_some(), "Muse must be a built-in adapter");
    let adapter = adapter.unwrap();
    let mut invocation = request(
        "muse",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.run_id = Some(crate::ids::RunId::new());
    invocation.identity.name = Some("muse-probe".into());
    invocation.identity.params = crate::agents::LaunchParams {
        channel: Some("probe".into()),
        model: Some("echo".into()),
        ..Default::default()
    };
    let mut inherited = crate::disk::paths::runtime_domain_env();
    inherited.extend(env(&[
        ("RIMZ_WORKSPACE_ID", "ws-probe"),
        ("RIMZ_PROJECT_ROOT", "/project"),
        ("TMUX_PANE", "%1"),
        ("ZELLIJ_PANE_ID", "1"),
    ]));
    let root = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(root.path());
    let runtime = RuntimePaths::under(id.clone(), root.path()).unwrap();
    let state = crate::disk::paths::StatePaths::under(id, root.path()).unwrap();
    let machine = crate::config::MachineConfig::default();
    for isolation in [
        crate::config::Isolation::Host,
        crate::config::Isolation::Scoped,
    ] {
        let plan =
            crate::harness::launch_plan::compile(crate::harness::launch_plan::LaunchPlanInputs {
                request: &invocation,
                cwd: root.path(),
                project_root: root.path(),
                rimz_bin: Path::new("/bin/rimz"),
                runtime: &runtime,
                state: &state,
                effective: None,
                commands: &machine.agents.commands,
                accounts: &machine.accounts,
                isolation,
                bwrap: (isolation.is_scoped()).then_some(Path::new("/bin/bwrap")),
                agents: Ok(&[]),
                ambient_env: &inherited,
                agent_shell: None,
                hibernate: false,
                recorded_system_prompt_digest: None,
            })
            .unwrap();
        let process = plan.process();
        let mut composed = inherited.clone();
        composed.extend(process.env.clone());
        for key in &process.unset {
            composed.remove(key);
        }
        assert_eq!(
            composed[crate::config::Isolation::ENV],
            isolation.to_string()
        );
        assert_eq!(plan.sandbox.is_some(), isolation.is_scoped());
        for key in composed.keys().filter(|key| {
            key.starts_with("RIMZ_")
                || crate::harness::hook_drain::HOOK_ENV_NAMES.contains(&key.as_str())
                || key.starts_with("XDG_")
                || key.as_str() == "HOME"
        }) {
            assert!(
                adapter.hook_env_keys().contains(&key.as_str()),
                "managed hooks lose {key}"
            );
        }
    }
}

#[test]
fn copilot_ask_refuses_nonmanual_defaults_without_gating_other_modes() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings.json");
    let login_env = BTreeMap::from([
        (
            "COPILOT_HOME".to_owned(),
            root.path().to_string_lossy().into_owned(),
        ),
        (
            "RIMZ_COPILOT_SETTINGS".to_owned(),
            settings.to_string_lossy().into_owned(),
        ),
    ]);
    for (mode, args, refused) in [
        ("assisted", vec!["--mode", "interactive"], true),
        ("allow-all", vec!["--mode", "interactive"], true),
        ("manual", vec!["--mode", "interactive"], false),
        ("allow-all", vec![], false),
        ("allow-all", vec!["--allow-all"], false),
        ("allow-all", vec!["--autopilot"], false),
    ] {
        std::fs::write(
            &settings,
            serde_json::json!({"defaultPermissionMode":mode}).to_string(),
        )
        .unwrap();
        let request = ExecRequest::bare_launch(
            AgentKind::new_unchecked("copilot"),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        );
        let result = compile_agent_process_with_extra_env(
            None,
            root.path(),
            &request,
            root.path(),
            &login_env,
            &LaunchReminders::default(),
        );
        if refused {
            match result {
                Err(AgentProcessCompileErr::SettingsConflict { kind, conflict }) => {
                    assert_eq!(kind, "copilot");
                    assert_eq!(conflict.path, settings);
                    assert_eq!(conflict.key, "defaultPermissionMode");
                    assert_eq!(conflict.value, serde_json::json!(mode));
                }
                other => panic!("{mode} {args:?}: expected settings refusal, got {other:?}"),
            }
        } else {
            assert!(result.is_ok(), "{mode} {args:?}: {result:?}");
        }
    }
}

#[test]
fn copilot_disabled_hooks_refuse_a_modeless_launch_only_when_rimz_hooks_are_installed() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings.json");
    std::fs::write(&settings, r#"{"disableAllHooks":true}"#).unwrap();
    let login_env = BTreeMap::from([
        (
            "COPILOT_HOME".to_owned(),
            root.path().to_string_lossy().into_owned(),
        ),
        (
            "RIMZ_COPILOT_SETTINGS".to_owned(),
            settings.to_string_lossy().into_owned(),
        ),
    ]);
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("copilot"), Vec::new());
    let compile = || {
        compile_agent_process_with_extra_env(
            None,
            root.path(),
            &request,
            root.path(),
            &login_env,
            &LaunchReminders::default(),
        )
    };
    let result = compile();
    assert!(result.is_ok(), "no RimZ hooks to silence: {result:?}");

    let adapter = crate::agents::definition_by_kind("copilot").unwrap();
    adapter
        .install_hooks(&login_env, Default::default())
        .unwrap();
    assert!(adapter.hooks_installed(&login_env));
    let result = compile();
    assert!(
        matches!(result, Err(AgentProcessCompileErr::SettingsConflict { ref conflict, .. }) if conflict.key == "disableAllHooks"),
        "{result:?}"
    );
}

#[test]
fn picker_argv_has_no_session_or_prompt_and_preserves_posture() {
    let action: Result<ExecAction, _> = serde_json::from_value(serde_json::json!({
        "action": "pick", "extra_args": ["--model", "chosen"]
    }));
    assert!(action.is_ok(), "picker action must decode: {action:?}");
    let action = action.unwrap();
    for (kind, expected) in [
        ("claude", vec!["claude", "--resume", "--model", "chosen"]),
        (
            "codex",
            vec![
                "codex",
                "resume",
                "--no-daemon",
                "-c",
                "tui.question_esc_back=false",
                "--model",
                "chosen",
            ],
        ),
        ("copilot", vec!["copilot", "--resume", "--model", "chosen"]),
        ("droid", vec!["droid", "--resume", "--model", "chosen"]),
        ("kimi", vec!["kimi", "--session", "--model", "chosen"]),
        (
            "kiro",
            vec![
                "kiro-cli",
                "chat",
                "--v3",
                "--resume-picker",
                "--model",
                "chosen",
            ],
        ),
        ("qwen", vec!["qwen", "--resume", "--model", "chosen"]),
        ("pi", vec!["pi", "--resume", "--model", "chosen"]),
    ] {
        let adapter = crate::agents::find_definition(kind).unwrap();
        assert_eq!(
            compile_provider_argv(adapter, kind, &action, Path::new("/repo")).unwrap(),
            expected
        );
    }
    let adapter = crate::agents::find_definition("grok").unwrap();
    assert!(compile_provider_argv(adapter, "grok", &action, Path::new("/repo")).is_err());
    let adapter = crate::agents::find_definition("claude").unwrap();
    let headless = crate::agents::HeadlessRequest {
        schema_open: false,
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: "/check/schema.json".into(),
        verdict_file: "/check/verdict.json".into(),
    };
    assert!(matches!(
        compile_provider_argv_with_headless(
            adapter,
            "claude",
            &action,
            Path::new("/repo"),
            Some(&headless),
            (Path::new("/repo"), &mut None),
        ),
        Err(AgentProcessCompileErr::NoHeadless { .. })
    ));
    let mut request = request("claude", action);
    request.identity.launch_id = Some("launch_picker".into());
    assert!(matches!(
        validate_exec_request(&request),
        Err(ExecWireErr::OrphanLaunchId)
    ));
    request.identity.name = Some("minted-handle".into());
    assert_eq!(round_trip(&request).1, request);
}

/// The temp unit and shared dir a compiled launch names in Environment.
fn temp_files(tmp: &str, caller: bool) -> Option<TempFiles> {
    Some(TempFiles {
        tmp: tmp.into(),
        shared: "/state/shared".into(),
        caller,
    })
}

/// The reminder a bare subagent launch carries: the child policy under Subagents.
fn child_reminder() -> String {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.subagent = true;
    let reminder = crate::harness::launch_reminders::render(
        &request,
        &LaunchReminders::default(),
        Path::new("/"),
        None,
    );
    assert!(reminder.contains("You are a subagent:"));
    reminder
}

fn env(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn launch_environment_key_literals_are_stable() {
    assert_eq!(ENV_RUN_ID, "RIMZ_RUN_ID");
    assert_eq!(ENV_AGENT_ID, "RIMZ_AGENT_ID");
    assert_eq!(ENV_AGENT_KIND, "RIMZ_AGENT_KIND");
    assert_eq!(ENV_AGENT_NAME, "RIMZ_AGENT_NAME");
    assert_eq!(ENV_AGENT_PROFILE, "RIMZ_AGENT_PROFILE");
    assert_eq!(ENV_AGENT_ROLE, "RIMZ_AGENT_ROLE");
    assert_eq!(ENV_TEAM, "RIMZ_TEAM");
    assert_eq!(ENV_LAUNCH_GROUP, "RIMZ_LAUNCH_GROUP");
    assert_eq!(ENV_LAUNCH_ORDINAL, "RIMZ_LAUNCH_ORDINAL");
    assert_eq!(ENV_AGENT_MODEL, "RIMZ_AGENT_MODEL");
    assert_eq!(ENV_AGENT_EFFORT, "RIMZ_AGENT_EFFORT");
    assert_eq!(ENV_AGENT_BUDGET, "RIMZ_AGENT_BUDGET");
}

#[test]
fn subagent_reminder_has_structural_wrapper_lines() {
    assert!(subagent_reminder().starts_with("<system_reminder>\n"));
    assert!(subagent_reminder().ends_with("\n</system_reminder>"));
}

#[test]
fn provider_compiler_preserves_action_and_trailing_argument_order() {
    let adapter = crate::agents::find_definition("codex").expect("codex adapter");
    let trailing = vec!["--model".to_owned(), "o3".to_owned()];
    for (action, verb, session) in [
        (
            ExecAction::Launch {
                prompt: Some("inspect".to_owned()),
                extra_args: trailing.clone(),
            },
            "inspect",
            None,
        ),
        (
            ExecAction::Fork {
                session_id: "fork-id".to_owned(),
                extra_args: trailing.clone(),
            },
            "fork",
            Some("fork-id"),
        ),
        (
            ExecAction::Resume {
                session_id: "resume-id".to_owned(),
                extra_args: trailing.clone(),
            },
            "resume",
            Some("resume-id"),
        ),
    ] {
        let argv = compile_provider_argv(adapter, "codex", &action, Path::new("/repo"))
            .expect("provider argv");
        assert!(argv.windows(2).any(|pair| pair == ["--model", "o3"]));
        assert!(argv.iter().any(|arg| arg == verb), "{argv:?}");
        assert!(session.is_none_or(|id| argv.iter().any(|arg| arg == id)));
        assert_eq!(argv.iter().filter(|arg| *arg == "--no-daemon").count(), 1);
        let flag = if session.is_some() { 3 } else { 1 };
        assert_eq!(argv[flag], "--no-daemon");
        assert_eq!(
            &argv[flag + 1..flag + 3],
            &["-c", "tui.question_esc_back=false"]
        );
        assert_eq!(&argv[flag + 3..flag + 5], &["--model", "o3"]);
    }
}

#[test]
fn headless_claude_provider_compiler_preserves_profile_and_schema_argv() {
    headless_provider_argv("claude");
}

#[test]
fn headless_codex_provider_compiler_preserves_profile_and_schema_argv() {
    headless_provider_argv("codex");
}

fn headless_provider_argv(kind: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut request = ExecRequest::bare_launch(
        AgentKind::new_unchecked(kind),
        vec!["--model".into(), "small".into()],
    );
    if let ExecAction::Launch { prompt, extra_args } = &mut request.action {
        *prompt = Some("Decide now.".into());
        if kind == "claude" {
            extra_args.extend(["--settings".into(), r#"{"theme":"dark"}"#.into()]);
        } else {
            extra_args.push("--no-daemon".into());
        }
    }
    request.headless = Some(crate::agents::HeadlessRequest {
        schema_open: false,
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: "/check/schema.json".into(),
        verdict_file: "/check/verdict.json".into(),
    });
    let process = compile_agent_process_with_extra_env(
        None,
        root.path(),
        &request,
        root.path(),
        &BTreeMap::new(),
        &LaunchReminders::default(),
    )
    .unwrap();
    let argv = process.provider_argv;
    if kind == "claude" {
        assert_eq!(argv.iter().filter(|arg| *arg == "--settings").count(), 1);
        let settings =
            crate::agents::PresetArgMatcher::Flag(vec!["--settings".into()]).occurrences(&argv);
        let value: serde_json::Value = serde_json::from_str(&settings[0].value).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["disableAllHooks"], true);
        insta::assert_json_snapshot!(argv, @r###"
            [
              "claude",
              "--model",
              "small",
              "--append-system-prompt",
              "<system_reminder>\n\n</system_reminder>",
              "--settings",
              "{\"disableAllHooks\":true,\"theme\":\"dark\"}",
              "-p",
              "--output-format",
              "json",
              "--json-schema",
              "{\"type\":\"object\",\"properties\":{\"pass\":{\"type\":\"boolean\"},\"reason\":{\"type\":\"string\"}},\"required\":[\"pass\",\"reason\"],\"additionalProperties\":false}",
              "--",
              "Decide now."
            ]
            "###);
    } else {
        insta::assert_json_snapshot!(argv, @r###"
            [
              "codex",
              "--model",
              "small",
              "-c",
              "developer_instructions=\"\"\"\n<system_reminder>\n\n</system_reminder>\"\"\"",
              "-c",
              "features.hooks=false",
              "exec",
              "--json",
              "--output-schema",
              "/check/schema.json",
              "-o",
              "/check/verdict.json",
              "--",
              "Decide now."
            ]
            "###);
    }
}

#[test]
fn headless_provider_compiler_refuses_unsupported_forms() {
    let root = tempfile::tempdir().unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("amp"), Vec::new());
    request.headless = Some(crate::agents::HeadlessRequest {
        schema_open: false,
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: root.path().join("schema.json"),
        verdict_file: root.path().join("verdict.json"),
    });
    let result = compile_agent_process_with_extra_env(
        None,
        root.path(),
        &request,
        root.path(),
        &BTreeMap::new(),
        &LaunchReminders::default(),
    );
    assert!(result.is_err(), "unsupported headless launch must refuse");
    assert!(result.unwrap_err().to_string().contains("amp"));
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
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity::default(),
    }
}

#[test]
fn provider_compiler_rejects_remote_codex_on_every_action() {
    for args in [vec!["--remote", "host:1"], vec!["--remote=host:1"]] {
        let extra_args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        for action in [
            ExecAction::Launch {
                prompt: None,
                extra_args: extra_args.clone(),
            },
            ExecAction::Resume {
                session_id: "sess-1".to_owned(),
                extra_args: extra_args.clone(),
            },
            ExecAction::Fork {
                session_id: "sess-1".to_owned(),
                extra_args: extra_args.clone(),
            },
        ] {
            let codex = crate::agents::find_definition("codex").unwrap();
            assert!(matches!(
                compile_provider_argv(codex, "codex", &action, Path::new("/repo")),
                Err(AgentProcessCompileErr::RejectedArg {
                    flag: "--remote",
                    ..
                })
            ));
            let claude = crate::agents::find_definition("claude").unwrap();
            assert!(compile_provider_argv(claude, "claude", &action, Path::new("/repo")).is_ok());
        }
    }
}

#[test]
fn text_prompt_sources_spill_before_encoding_the_exec_wire() {
    use crate::config::PromptSource;
    use crate::harness::team_prompt::{Consensus, TeamPrompt};

    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(dir.path());
    let texts = [
        "private base text",
        "private craft text",
        "private pipeline text",
    ];
    let source = |index: usize| PromptSource::Text {
        origin: PathBuf::from(format!("source-{index}.md")),
        text: texts[index].to_owned(),
    };
    let mut input = request(
        "claude",
        ExecAction::Launch {
            prompt: Some("private task text".to_owned()),
            extra_args: Vec::new(),
        },
    );
    input.system_prompt_file = Some(source(0));
    input.append_system_prompt_files = vec![source(1)];
    input.team_prompt = Some(TeamPrompt {
        consensus: Consensus::BuiltIn,
        files: vec![source(2)],
    });
    assert_eq!(
        serde_json::from_str::<ExecRequest>(&serde_json::to_string(&input).unwrap()).unwrap(),
        input
    );
    let plan = plan_exec_argv(Path::new("/bin/rimz"), &runtime, &input).unwrap();
    let payload = plan
        .argv
        .windows(2)
        .find(|pair| pair[0] == "--request")
        .unwrap()[1]
        .as_str();
    assert!(!payload.contains("private"));
    assert_eq!(plan.artifacts.len(), 4);
    assert_eq!(plan.artifacts[0].contents, "private task text");
    for (artifact, text) in plan.artifacts[1..].iter().zip(texts) {
        assert_eq!(artifact.contents, text);
        assert_eq!(artifact.path, prompt_artifact_path(&runtime, "frag", text));
        assert!(!artifact.path.exists());
    }
    let wire: ExecWire = serde_json::from_str(payload).unwrap();
    let decoded = wire.request;
    let sources = decoded
        .system_prompt_file
        .iter()
        .chain(&decoded.append_system_prompt_files)
        .chain(&decoded.team_prompt.as_ref().unwrap().files);
    for (source, artifact) in sources.zip(&plan.artifacts[1..]) {
        assert_eq!(source, &PromptSource::File(artifact.path.clone()));
    }
    exec_argv(Path::new("/bin/rimz"), &runtime, &input).unwrap();
    for artifact in plan.artifacts {
        assert_eq!(
            std::fs::read_to_string(artifact.path).unwrap(),
            artifact.contents
        );
    }
}

fn team() -> crate::harness::launch_reminders::TeamReminder {
    let role = |role: &str, profile: &str| crate::config::RoleBinding {
        signals: Vec::new(),
        owns: Vec::new(),
        flip_compact: None,
        idle_compact: None,
        keep_warm: None,
        auto_compact: None,
        role: role.to_owned(),
        profile: profile.to_owned(),
        mode: None,
        model: None,
        effort: None,
        fast: None,
        budget: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        args: None,
    };
    crate::harness::launch_reminders::TeamReminder::new(crate::config::Team {
        keep_warm: None,
        roles: vec![role("planner", "claude"), role("coder", "codex")],
        leader: Some("planner".to_owned()),
        layout: None,
        scratch_files: Some(vec!["blackboard.md".to_owned()]),
        consensus_file: None,
        append_system_prompt_files: Vec::new(),
        stages: Vec::new(),
    })
}

fn team_request(kind: &str) -> ExecRequest {
    let mut invocation = request(
        kind,
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.identity.params = crate::agents::LaunchParams {
        team: Some("forge".to_owned()),
        role: Some("coder".to_owned()),
        channel: Some("feature".to_owned()),
        ..crate::agents::LaunchParams::default()
    };
    invocation
}

fn round_trip(input: &ExecRequest) -> (Vec<String>, ExecRequest) {
    let dir = tempfile::tempdir().expect("runtime root");
    let runtime = runtime(dir.path());
    let plan = plan_exec_argv(Path::new("/bin/rimz"), &runtime, input).expect("plan exec request");
    for artifact in &plan.artifacts {
        assert!(!artifact.path.exists());
        let ExecAction::Launch { prompt, .. } = &input.action else {
            panic!("only launch requests carry prompt artifacts");
        };
        assert_eq!(Some(&artifact.contents), prompt.as_ref());
    }
    let argv = exec_argv(Path::new("/bin/rimz"), &runtime, input).expect("encode exec request");
    assert_eq!(plan.argv, argv);
    let payload = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
        .expect("request payload");
    let decoded = decode_exec_request(input.kind.as_str(), input.worktree_path.as_deref(), payload)
        .expect("decode exec request");
    (argv, decoded)
}

fn runtime(root: &Path) -> RuntimePaths {
    RuntimePaths::under(crate::ids::WorkspaceId::from_project_root(root), root)
        .expect("runtime paths")
}

#[test]
fn provider_prompt_size_is_checked_at_the_argv_boundary() {
    let adapter = crate::agents::find_definition("codex").expect("codex");
    for size in [TEXT_PROMPT_LIMIT, TEXT_PROMPT_LIMIT + 1] {
        let result = compile_provider_argv(
            adapter,
            "codex",
            &ExecAction::Launch {
                prompt: Some("x".repeat(size)),
                extra_args: Vec::new(),
            },
            Path::new("/repo"),
        );
        if size == TEXT_PROMPT_LIMIT {
            assert!(result.is_ok());
        } else {
            assert!(matches!(
                result,
                Err(AgentProcessCompileErr::PromptTooLarge { size: actual, limit: TEXT_PROMPT_LIMIT, .. }) if actual == size
            ));
        }
    }
}

#[test]
fn positional_prompts_cannot_be_mistaken_for_options() {
    let adapter = crate::agents::find_definition("kiro").unwrap();
    for kind in ["kiro", "custom-positional"] {
        let result = compile_provider_argv(
            adapter,
            kind,
            &ExecAction::Launch {
                prompt: Some("- fix it".to_owned()),
                extra_args: Vec::new(),
            },
            Path::new("/repo"),
        );
        assert!(
            matches!(&result, Err(AgentProcessCompileErr::PositionalPromptOption { kind: rejected }) if rejected == kind)
        );
        let message = result.unwrap_err().to_string();
        assert!(message.contains("rimz message"));
        assert!(message.contains("start the prompt with a word"));
    }
    let argv = compile_provider_argv(
        adapter,
        "kiro",
        &ExecAction::Launch {
            prompt: Some("fix - it".to_owned()),
            extra_args: Vec::new(),
        },
        Path::new("/repo"),
    )
    .unwrap();
    assert_eq!(argv.last().map(String::as_str), Some("fix - it"));
    assert!(
        compile_provider_argv(
            adapter,
            "kiro",
            &ExecAction::Resume {
                session_id: "sess_redacted".to_owned(),
                extra_args: Vec::new(),
            },
            Path::new("/repo")
        )
        .is_ok()
    );
}

#[test]
fn process_compiler_composes_adapter_and_identity_environment() {
    let project = tempfile::tempdir().expect("project");
    let params = crate::agents::LaunchParams {
        channel: Some("design".to_owned()),
        ..Default::default()
    };
    let mut invocation = request(
        "copilot",
        ExecAction::Launch {
            prompt: Some("inspect".to_owned()),
            extra_args: Vec::new(),
        },
    );
    invocation.run_id = Some(
        "run_0123456789abcdef0123456789abcdef"
            .parse()
            .expect("run id"),
    );
    invocation.identity.params = params;

    let process = compile_agent_process(project.path(), &invocation, project.path(), None)
        .expect("compiled process");

    assert_eq!(process.provider_program, "copilot");
    assert_eq!(
        process
            .env
            .get("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT")
            .map(String::as_str),
        Some("false")
    );
    assert_eq!(
        process
            .env
            .get(crate::harness::launch::ENV_AGENT_KIND)
            .map(String::as_str),
        Some("copilot")
    );
    assert_eq!(
        process
            .env
            .get(crate::workspace::ENV_CHANNEL)
            .map(String::as_str),
        Some("design")
    );
}

#[test]
fn process_compiler_locks_down_only_subagent_launches() {
    let project = tempfile::tempdir().expect("project");
    for (kind, profile_args, expected_suffix) in [
        (
            "claude",
            vec![
                "--disallowedTools".to_owned(),
                "Read".to_owned(),
                "Agent(fork)".to_owned(),
            ],
            vec![
                "--disallowedTools".to_owned(),
                "Read".to_owned(),
                "Agent".to_owned(),
                "--append-system-prompt".to_owned(),
                child_reminder(),
                "--".to_owned(),
                "inspect".to_owned(),
            ],
        ),
        (
            "codex",
            vec!["-c".to_owned(), "features.multi_agent=true".to_owned()],
            vec![
                "-c".to_owned(),
                "features.multi_agent=false".to_owned(),
                "-c".to_owned(),
                format!(
                    "developer_instructions={}",
                    toml::Value::String(child_reminder())
                ),
                "--".to_owned(),
                "inspect".to_owned(),
            ],
        ),
    ] {
        let mut invocation = request(
            kind,
            ExecAction::Launch {
                prompt: Some("inspect".to_owned()),
                extra_args: profile_args.clone(),
            },
        );
        let ordinary = compile_agent_process(project.path(), &invocation, project.path(), None)
            .expect("ordinary process");
        let args_start = 1 + crate::agents::find_definition(kind)
            .unwrap()
            .spec()
            .launch
            .fixed_args
            .len();
        assert_eq!(
            ordinary.provider_argv[args_start..args_start + profile_args.len()],
            profile_args
        );

        invocation.subagent = true;
        let child = compile_agent_process(project.path(), &invocation, project.path(), None)
            .expect("subagent process");
        assert!(child.provider_argv.ends_with(&expected_suffix));
    }
}

#[test]
fn scoped_launch_keeps_codex_sandbox_and_grants_room_roots() {
    let project = tempfile::tempdir().expect("project");
    for (kind, args) in [
        (
            "codex",
            vec![
                "--ask-for-approval",
                "on-request",
                "-c",
                r#"approvals_reviewer="auto_review""#,
            ],
        ),
        ("claude", vec!["--permission-mode", "auto"]),
    ] {
        let invocation = request(
            kind,
            ExecAction::Resume {
                session_id: "session".to_owned(),
                extra_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            },
        );
        let host = compile_agent_process(project.path(), &invocation, project.path(), None)
            .expect("host process");
        let reminders = LaunchReminders {
            sandbox: true,
            permission_roots: Some(["/state".into(), "/runtime".into(), "/shared-runtime".into()]),
            ..LaunchReminders::default()
        };
        let sandboxed = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &invocation,
            project.path(),
            &BTreeMap::new(),
            &reminders,
        )
        .expect("sandboxed process");

        let sandbox_values: Vec<_> = sandboxed
            .provider_argv
            .windows(2)
            .filter(|pair| pair[0] == "--sandbox")
            .map(|pair| pair[1].as_str())
            .collect();
        if kind == "codex" {
            assert!(sandbox_values.is_empty());
            assert!(
                sandboxed.provider_argv.iter().any(|arg| {
                    arg.starts_with("sandbox_workspace_write.writable_roots=")
                        && arg.contains("/state")
                        && arg.contains("/runtime")
                        && arg.contains("/shared-runtime")
                }),
                "{:?}",
                sandboxed.provider_argv
            );
            assert!(!host.provider_argv.contains(&"--sandbox".to_owned()));
            assert!(
                !host
                    .provider_argv
                    .iter()
                    .any(|arg| arg.starts_with("default_permissions="))
            );
            assert!(
                !sandboxed
                    .provider_argv
                    .iter()
                    .any(|arg| arg.starts_with("default_permissions="))
            );
            for process in [&host, &sandboxed] {
                assert!(
                    process
                        .provider_argv
                        .windows(2)
                        .any(|pair| pair == ["--ask-for-approval", "on-request"])
                );
                assert!(
                    process
                        .provider_argv
                        .contains(&r#"approvals_reviewer="auto_review""#.to_owned())
                );
                assert!(
                    !process
                        .provider_argv
                        .iter()
                        .any(|arg| arg.starts_with("permissions."))
                );
            }
        } else {
            assert!(sandbox_values.is_empty());
            assert!(
                sandboxed
                    .provider_argv
                    .windows(2)
                    .any(|pair| pair == ["--permission-mode", "auto"]),
                "scoped settings must preserve Claude's approval mode"
            );
        }
    }
}

#[test]
fn scoped_codex_preserves_each_permission_selection() {
    let project = tempfile::tempdir().unwrap();
    for (selection, grant) in [
        (vec![], Some("sandbox_workspace_write.writable_roots=")),
        (
            vec!["--sandbox", "workspace-write"],
            Some("sandbox_workspace_write.writable_roots="),
        ),
        (
            vec!["--approve-for-me"],
            Some("sandbox_workspace_write.writable_roots="),
        ),
        (
            vec!["--not-so-yolo"],
            Some("sandbox_workspace_write.writable_roots="),
        ),
        (vec!["--sandbox", "read-only"], None),
        (vec!["--sandbox", "danger-full-access"], None),
        (vec!["-c", "default_permissions=':read-only'"], None),
        (
            vec!["-c", "default_permissions=':danger-full-access'"],
            None,
        ),
        (
            vec!["-c", "default_permissions=':workspace'"],
            Some("--add-dir"),
        ),
        (
            vec!["-c", "default_permissions='dev'"],
            Some("permissions.dev.workspace_roots="),
        ),
        (vec!["--dangerously-bypass-approvals-and-sandbox"], None),
    ] {
        let invocation = request(
            "codex",
            ExecAction::Launch {
                prompt: None,
                extra_args: selection.iter().map(|arg| (*arg).to_owned()).collect(),
            },
        );
        let process = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &invocation,
            project.path(),
            &BTreeMap::from([("CODEX_HOME".into(), project.path().display().to_string())]),
            &LaunchReminders {
                sandbox: true,
                permission_roots: Some([
                    "/state".into(),
                    "/runtime".into(),
                    "/shared-runtime".into(),
                ]),
                ..Default::default()
            },
        )
        .unwrap();
        let argv = &process.provider_argv;
        let start = 1 + crate::agents::definition_by_kind("codex")
            .unwrap()
            .spec()
            .launch
            .fixed_args
            .len();
        assert_eq!(&argv[start..start + selection.len()], selection, "{argv:?}");
        let grants: Vec<_> = argv
            .iter()
            .filter(|arg| {
                arg.starts_with("sandbox_workspace_write.writable_roots=")
                    || arg.starts_with("permissions.dev.workspace_roots=")
                    || *arg == "--add-dir"
            })
            .collect();
        if let Some(grant) = grant {
            assert!(grants.iter().any(|arg| arg.starts_with(grant)), "{argv:?}");
            for root in ["/state", "/runtime", "/shared-runtime"] {
                assert!(argv.iter().any(|arg| arg.contains(root)), "{argv:?}");
            }
        } else {
            assert!(grants.is_empty(), "{argv:?}");
        }
    }
}

#[test]
fn claude_sandbox_settings_merge_at_every_level_with_or_without_routine_permissions() {
    let project = tempfile::tempdir().unwrap();
    for isolation in [
        crate::config::Isolation::Host,
        crate::config::Isolation::Scoped,
        crate::config::Isolation::Sandbox,
    ] {
        for routine in [false, true] {
            let invocation = request("claude", ExecAction::Launch {
                prompt: None,
                extra_args: vec!["--settings".into(), r#"{"sandbox":{"excludedCommands":["custom *"],"filesystem":{"allowWrite":["/user"]},"network":{"allowedDomains":["example.com"]}}}"#.into()],
            });
            let roots = ["/state".into(), "/runtime".into(), "/shared-runtime".into()];
            let process = compile_agent_process_with_extra_env(
                None,
                project.path(),
                &invocation,
                project.path(),
                &BTreeMap::from([
                    (
                        "CLAUDE_CONFIG_DIR".into(),
                        project.path().display().to_string(),
                    ),
                    (crate::config::Isolation::ENV.into(), isolation.to_string()),
                ]),
                &LaunchReminders {
                    sandbox: isolation.is_scoped(),
                    permission_roots: Some(roots.clone()),
                    settings: Some((project.path().to_path_buf(), BTreeMap::new())),
                    routine_rimz: routine.then(|| {
                        (
                            project.path().to_path_buf(),
                            ["/tmp".into(), "/shared".into()],
                        )
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
            let args = crate::agents::PresetArgMatcher::Flag(vec!["--settings".into()])
                .occurrences(&process.provider_argv);
            assert_eq!(args.len(), 1);
            let settings: serde_json::Value = serde_json::from_str(&args[0].value).unwrap();
            let sandbox = &settings["sandbox"];
            let writes = sandbox["filesystem"]["allowWrite"].as_array().unwrap();
            for root in &roots {
                assert!(writes.contains(&serde_json::json!(root)), "{settings}");
            }
            assert!(writes.contains(&serde_json::json!("/user")));
            assert_eq!(
                sandbox["network"]["allowedDomains"],
                serde_json::json!(["example.com"])
            );
            assert_eq!(
                sandbox.get("enableWeakerNestedSandbox"),
                isolation
                    .is_scoped()
                    .then_some(&serde_json::Value::Bool(true))
            );
            assert_eq!(
                sandbox.get("enabled"),
                (isolation == crate::config::Isolation::Sandbox)
                    .then_some(&serde_json::Value::Bool(true))
            );
            assert_eq!(
                sandbox.as_object().unwrap().len(),
                3 + usize::from(isolation != crate::config::Isolation::Host)
            );
            let excluded = sandbox["excludedCommands"].as_array().unwrap();
            assert!(excluded.contains(&serde_json::json!("custom *")));
            for prefix in ["rimz message", "rimz agents", "rimz subagents"] {
                assert_eq!(
                    excluded.contains(&serde_json::json!(format!("{prefix} *"))),
                    routine,
                    "{settings}"
                );
            }
        }
    }
}

#[test]
fn configured_lsp_denies_native_tool_on_every_launch_action() {
    for action in [
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        ExecAction::Resume {
            session_id: "session".into(),
            extra_args: Vec::new(),
        },
        ExecAction::Fork {
            session_id: "session".into(),
            extra_args: Vec::new(),
        },
    ] {
        for subagent in [false, true] {
            for configured in [false, true] {
                let mut invocation = request("claude", action.clone());
                invocation.subagent = subagent;
                let process = compile_agent_process_with_extra_env(
                    None,
                    Path::new("/checkout"),
                    &invocation,
                    Path::new("/checkout"),
                    &BTreeMap::new(),
                    &LaunchReminders {
                        lsp_configured: configured,
                        ..LaunchReminders::default()
                    },
                )
                .expect("process");
                assert_eq!(
                    process.provider_argv.iter().any(|arg| arg == "LSP"),
                    configured
                );
                assert_eq!(
                    process.provider_argv.iter().any(|arg| arg == "Agent"),
                    subagent
                );
            }
        }
    }
}

#[test]
fn a_launch_without_a_team_does_not_inherit_its_callers_team() {
    let inherited = env(&[(ENV_TEAM, "caller-team")]);
    for (team, role) in [
        (None, None),
        (None, Some("inline")),
        (Some("forge"), Some("coder")),
    ] {
        let mut invocation = request(
            "claude",
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        );
        invocation.identity.params.team = team.map(str::to_owned);
        invocation.identity.params.role = role.map(str::to_owned);
        for shell in ["/bin/sh", "/bin/zsh"]
            .into_iter()
            .filter(|shell| Path::new(shell).is_file())
        {
            let process = compile_agent_process_with_extra_env(
                None,
                Path::new("/checkout"),
                &invocation,
                Path::new("/checkout"),
                &inherited,
                &LaunchReminders {
                    agent_shell: Some(shell.into()),
                    ..LaunchReminders::default()
                },
            )
            .expect("process");
            let command = process.login_shell_argv(&argv(&[
                "/bin/sh",
                "-c",
                "printf '%s' \"${RIMZ_TEAM-unset}\"",
            ]));
            let output = Command::new(&command[0])
                .args(&command[1..])
                .envs(&inherited)
                .envs(&process.env)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                team.unwrap_or("unset"),
                "{shell}"
            );
            if let Some(role) = role {
                assert_eq!(process.env[ENV_AGENT_ROLE], role);
            }
        }
    }
}

#[test]
fn an_unstamped_launch_never_carries_the_runtime_switch() {
    let switch = BTreeMap::from([(ENV_RUNTIME_ENV.to_owned(), "1".to_owned())]);
    for stamped in [false, true] {
        let process = compile_agent_process_with_extra_env(
            None,
            Path::new("/checkout"),
            &request(
                "claude",
                ExecAction::Launch {
                    prompt: None,
                    extra_args: Vec::new(),
                },
            ),
            Path::new("/checkout"),
            &switch,
            &LaunchReminders {
                runtime_env: stamped,
                ..LaunchReminders::default()
            },
        )
        .expect("process");
        assert_eq!(process.env.contains_key(ENV_RUNTIME_ENV), stamped);
        assert_eq!(process.unset.contains(ENV_RUNTIME_ENV), !stamped);
    }
}

#[test]
fn process_compiler_appends_subagent_reminder_for_native_adapters() {
    let project = tempfile::tempdir().expect("project");
    for kind in ["claude", "qwen", "droid"] {
        let mut invocation = request(
            kind,
            ExecAction::Launch {
                prompt: Some("inspect".to_owned()),
                extra_args: Vec::new(),
            },
        );

        let ordinary = compile_agent_process(project.path(), &invocation, project.path(), None)
            .expect("ordinary process");
        assert!(
            !ordinary
                .provider_argv
                .iter()
                .any(|arg| arg == &child_reminder()),
            "{kind}: {:?}",
            ordinary.provider_argv
        );

        invocation.subagent = true;
        let child = compile_agent_process(project.path(), &invocation, project.path(), None)
            .expect("subagent process");
        assert!(
            child
                .provider_argv
                .windows(2)
                .any(|args| args == ["--append-system-prompt", child_reminder().as_str()]),
            "{kind}: {:?}",
            child.provider_argv
        );
    }

    let mut invocation = request(
        "codex",
        ExecAction::Launch {
            prompt: Some("inspect".to_owned()),
            extra_args: Vec::new(),
        },
    );
    invocation.subagent = true;
    let child = compile_agent_process(project.path(), &invocation, project.path(), None)
        .expect("codex subagent process");
    let occurrences = crate::agents::PresetArgMatcher::ConfigKey {
        flags: vec!["-c".to_owned(), "--config".to_owned()],
        key: "developer_instructions".to_owned(),
    }
    .occurrences(&child.provider_argv);

    assert_eq!(occurrences.len(), 1);
    assert_eq!(
        parse_toml_string_or_raw(&occurrences[0].value),
        child_reminder()
    );
}

#[test]
fn process_compiler_appends_available_catalog_only_to_peer_launches() {
    let project = tempfile::tempdir().expect("project");
    let catalog = crate::harness::subagent_policy::SubagentCatalog::Available(vec![
        crate::harness::subagent_policy::SubagentProfile {
            name: "explorer".to_owned(),
            source: crate::harness::subagent_policy::SubagentProfileSource::Profile,
            agent: Some("claude".to_owned()),
            model: Some("sonnet".to_owned()),
            effort: None,
            description: None,
        },
    ]);
    let reminder = crate::harness::subagent_policy::reminder(&catalog);

    for kind in ["claude", "codex"] {
        let invocation = request(
            kind,
            ExecAction::Launch {
                prompt: Some("inspect".to_owned()),
                extra_args: Vec::new(),
            },
        );
        let peer = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &invocation,
            project.path(),
            &BTreeMap::new(),
            &LaunchReminders {
                subagent_catalog: Some(catalog.clone()),
                ..LaunchReminders::default()
            },
        )
        .expect("peer process");
        if kind == "claude" {
            assert!(
                peer.provider_argv
                    .windows(2)
                    .any(|args| args[0] == "--append-system-prompt" && args[1].contains(&reminder))
            );
        } else {
            let occurrences = crate::agents::PresetArgMatcher::ConfigKey {
                flags: vec!["-c".to_owned(), "--config".to_owned()],
                key: "developer_instructions".to_owned(),
            }
            .occurrences(&peer.provider_argv);
            assert_eq!(occurrences.len(), 1);
            assert!(parse_toml_string_or_raw(&occurrences[0].value).contains(&reminder));
        }

        let without_catalog =
            compile_agent_process(project.path(), &invocation, project.path(), None)
                .expect("process without catalog");
        assert!(
            !without_catalog
                .provider_argv
                .iter()
                .any(|arg| arg.contains(&reminder))
        );

        let mut child = invocation;
        child.subagent = true;
        let child = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &child,
            project.path(),
            &BTreeMap::new(),
            &LaunchReminders {
                subagent_catalog: Some(catalog.clone()),
                ..LaunchReminders::default()
            },
        )
        .expect("child process");
        assert!(
            child
                .provider_argv
                .iter()
                .any(|arg| arg.contains(&child_reminder()))
        );
        assert!(
            !child
                .provider_argv
                .iter()
                .any(|arg| arg.contains(&reminder))
        );
    }
}

#[test]
fn process_compiler_appends_team_context_for_native_adapters() {
    let project = tempfile::tempdir().expect("project");
    let team = team();
    // Each adapter carries the same team facts, without provider or model names.
    let rendered = |invocation: &ExecRequest| {
        crate::harness::launch_reminders::render(
            invocation,
            &LaunchReminders {
                team: Some(team.clone()),
                ..LaunchReminders::default()
            },
            project.path(),
            None,
        )
    };

    for kind in ["claude", "qwen", "droid"] {
        let invocation = team_request(kind);
        let reminder = rendered(&invocation);
        let process = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &invocation,
            project.path(),
            &BTreeMap::new(),
            &LaunchReminders {
                team: Some(team.clone()),
                ..LaunchReminders::default()
            },
        )
        .expect("process");
        assert!(
            process
                .provider_argv
                .windows(2)
                .any(|args| args == ["--append-system-prompt", reminder.as_str()]),
            "{kind}: append-system-prompt reminder missing"
        );
    }

    let invocation = team_request("codex");
    let reminder = rendered(&invocation);
    assert!(
        reminder.contains("Members: @planner, @coder (you)."),
        "{reminder}"
    );
    let process = compile_agent_process_with_extra_env(
        None,
        project.path(),
        &invocation,
        project.path(),
        &BTreeMap::new(),
        &LaunchReminders {
            team: Some(team.clone()),
            ..LaunchReminders::default()
        },
    )
    .expect("codex process");
    let occurrences = crate::agents::PresetArgMatcher::ConfigKey {
        flags: vec!["-c".to_owned(), "--config".to_owned()],
        key: "developer_instructions".to_owned(),
    }
    .occurrences(&process.provider_argv);
    assert_eq!(occurrences.len(), 1);
    assert_eq!(parse_toml_string_or_raw(&occurrences[0].value), reminder);
}

#[test]
fn process_compiler_joins_catalog_and_team_context_in_one_occurrence() {
    let project = tempfile::tempdir().expect("project");
    let catalog = crate::harness::subagent_policy::SubagentCatalog::Available(vec![
        crate::harness::subagent_policy::SubagentProfile {
            name: "explorer".to_owned(),
            source: crate::harness::subagent_policy::SubagentProfileSource::Profile,
            agent: Some("claude".to_owned()),
            model: None,
            effort: None,
            description: None,
        },
    ]);
    let catalog_reminder = crate::harness::subagent_policy::reminder(&catalog);
    let team = team();

    for kind in ["claude", "codex"] {
        let mut invocation = team_request(kind);
        invocation.identity.params.model = Some("gpt-6-astra".to_owned());
        invocation.identity.params.effort = Some("high".to_owned());
        let reminders = LaunchReminders {
            subagent_catalog: Some(catalog.clone()),
            team: Some(team.clone()),
            ..LaunchReminders::default()
        };
        let team_reminder = crate::harness::launch_reminders::render(
            &invocation,
            &LaunchReminders {
                team: Some(team.clone()),
                ..LaunchReminders::default()
            },
            project.path(),
            None,
        );
        let team_reminder = team_reminder
            .trim_start_matches("<system_reminder>\n")
            .trim_end_matches("\n</system_reminder>")
            .to_owned();
        assert!(
            team_reminder.contains("Members: @planner, @coder (you)."),
            "{team_reminder}"
        );
        let process = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &invocation,
            project.path(),
            &BTreeMap::new(),
            &reminders,
        )
        .expect("process");
        let channel = crate::agents::find_definition(kind)
            .and_then(|adapter| adapter.append_system_text_channel())
            .expect("system text matcher");
        let matcher = super::argv_matcher(&channel).expect("argv channel");
        let occurrences = matcher.occurrences(&process.provider_argv);
        assert_eq!(
            occurrences.len(),
            1,
            "{kind}: expected one appended system-text occurrence"
        );
        assert_eq!(occurrences[0].value.matches("<system_reminder>").count(), 1);
        assert_eq!(
            parse_toml_string_or_raw(&occurrences[0].value),
            wrap(&format!("{team_reminder}\n\n{catalog_reminder}"))
        );
    }
}

#[test]
fn process_compiler_joins_sandbox_reminder_for_native_peers_and_children() {
    let project = tempfile::tempdir().expect("project");
    for kind in ["claude", "qwen", "droid", "codex"] {
        for subagent in [false, true] {
            let mut invocation = team_request(kind);
            invocation.subagent = subagent;
            invocation.identity.params.model = Some("gpt-6-astra".to_owned());
            let reminders = LaunchReminders {
                sandbox: true,
                files: temp_files("/tmp", subagent),
                team: Some(team()),
                subagent_catalog: Some(crate::harness::subagent_policy::SubagentCatalog::Disabled),
                ..LaunchReminders::default()
            };
            let process = compile_agent_process_with_extra_env(
                None,
                project.path(),
                &invocation,
                project.path(),
                &BTreeMap::new(),
                &reminders,
            )
            .expect("process");
            let channel = crate::agents::find_definition(kind)
                .and_then(|adapter| adapter.append_system_text_channel())
                .expect("system text channel");
            let occurrences = super::argv_matcher(&channel)
                .expect("argv channel")
                .occurrences(&process.provider_argv);
            assert_eq!(occurrences.len(), 1);
            let text = parse_toml_string_or_raw(&occurrences[0].value);
            assert_eq!(text.matches("<system_reminder>").count(), 1);
            assert_eq!(text.matches("</system_reminder>").count(), 1);
            assert!(text.contains("- tmp: /tmp (`$TMPDIR`): every temporary file you make."));
            assert_eq!(text.contains("You are a subagent:"), subagent);
            assert_eq!(
                text,
                crate::harness::launch_reminders::render(
                    &invocation,
                    &reminders,
                    project.path(),
                    None
                )
            );
        }
    }
}

#[test]
fn process_compiler_appends_model_line_for_native_adapters() {
    let project = tempfile::tempdir().expect("project");
    for kind in ["claude", "codex"] {
        for subagent in [false, true] {
            for model in [false, true] {
                let mut invocation = team_request(kind);
                invocation.subagent = subagent;
                invocation.identity.params.model = Some("gpt-6-astra".to_owned());
                invocation.identity.params.effort = Some("high".to_owned());
                let process = compile_agent_process_with_extra_env(
                    None,
                    project.path(),
                    &invocation,
                    project.path(),
                    &BTreeMap::new(),
                    &LaunchReminders {
                        model,
                        env: false,
                        sandbox: false,
                        team: Some(team()),
                        subagent_catalog: Some(
                            crate::harness::subagent_policy::SubagentCatalog::Disabled,
                        ),
                        ..LaunchReminders::default()
                    },
                )
                .expect("process");
                let channel = crate::agents::find_definition(kind)
                    .and_then(|adapter| adapter.append_system_text_channel())
                    .expect("system text channel");
                let occurrences = super::argv_matcher(&channel)
                    .expect("argv channel")
                    .occurrences(&process.provider_argv);
                assert_eq!(occurrences.len(), 1);
                let text = parse_toml_string_or_raw(&occurrences[0].value);
                assert_eq!(text.matches("<system_reminder>").count(), 1);
                assert_eq!(text.matches("</system_reminder>").count(), 1);
                assert_eq!(text.contains("GPT 6 Astra."), subagent && model);
                assert!(!text.contains("high effort"));
                assert_eq!(text.contains("You are a subagent:"), subagent);
                assert_eq!(text.contains("team `forge`"), !subagent);
                assert_eq!(text.contains("Subagents are disabled"), !subagent);
                if subagent && model {
                    assert!(text.contains("You are @coder, running on GPT 6 Astra"));
                    assert!(
                        text.find("GPT 6 Astra").unwrap()
                            < text.find("You are a subagent:").unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn process_compiler_carries_only_the_temp_lines_when_nothing_else_applies() {
    let project = tempfile::tempdir().expect("project");
    let mut invocation = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.identity.params.model = Some("fable".to_owned());
    let process = compile_agent_process_with_extra_env(
        None,
        project.path(),
        &invocation,
        project.path(),
        &BTreeMap::new(),
        &LaunchReminders {
            model: false,
            files: temp_files("/state/tmp/otter", false),
            ..LaunchReminders::default()
        },
    )
    .expect("process");
    assert!(process.provider_argv.contains(&process.reminder));
    assert!(
        process
            .reminder
            .contains("- tmp: /state/tmp/otter (`$TMPDIR`)")
    );
    assert!(
        process
            .reminder
            .contains("- shared: /state/shared (`$RIMZ_SHARED`)")
    );
    assert!(!process.reminder.contains("You are"));
}

#[test]
fn process_compiler_carries_reminders_in_extension_env_off_argv() {
    let project = tempfile::tempdir().expect("project");
    let team = team();
    for kind in ["pi", "opencode"] {
        let invocation = team_request(kind);
        let reminders = LaunchReminders {
            team: Some(team.clone()),
            sandbox: true,
            files: temp_files("/tmp", false),
            ..LaunchReminders::default()
        };
        let process = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &invocation,
            project.path(),
            &BTreeMap::new(),
            &reminders,
        )
        .expect("process");
        let reminder =
            crate::harness::launch_reminders::render(&invocation, &reminders, project.path(), None);
        assert!(reminder.contains("- tmp: /tmp (`$TMPDIR`)"), "{kind}");
        assert_eq!(process.env[ENV_LAUNCH_REMINDERS], reminder, "{kind}");
        assert!(
            process
                .provider_argv
                .iter()
                .all(|arg| !arg.contains("system_reminder")),
            "{kind}: reminder leaked into argv"
        );

        let bare = compile_agent_process_with_extra_env(
            None,
            project.path(),
            &invocation,
            project.path(),
            &BTreeMap::new(),
            &LaunchReminders {
                model: false,
                files: temp_files("/tmp", false),
                ..LaunchReminders::default()
            },
        )
        .expect("process without reminders");
        assert!(bare.reminder.contains("- tmp: /tmp"), "{kind}");
        assert_eq!(bare.env[ENV_LAUNCH_REMINDERS], bare.reminder, "{kind}");
    }
}

#[test]
fn process_compiler_merges_subagent_reminder_into_existing_append_flag() {
    let project = tempfile::tempdir().expect("project");
    let mut invocation = request(
        "claude",
        ExecAction::Launch {
            prompt: Some("inspect".to_owned()),
            extra_args: vec!["--append-system-prompt=existing guidance".to_owned()],
        },
    );
    invocation.subagent = true;

    let child = compile_agent_process(project.path(), &invocation, project.path(), None)
        .expect("subagent process");
    let matcher =
        crate::agents::PresetArgMatcher::TextFlag(vec!["--append-system-prompt".to_owned()]);
    let occurrences = matcher.occurrences(&child.provider_argv);

    assert_eq!(occurrences.len(), 1, "{:?}", child.provider_argv);
    assert_eq!(
        occurrences[0].value,
        format!("existing guidance\n\n{}", child_reminder())
    );
}

#[test]
fn process_compiler_merges_codex_reminder_by_config_key() {
    let project = tempfile::tempdir().expect("project");
    for developer_args in [
        vec![
            "-c".to_owned(),
            "developer_instructions=existing".to_owned(),
        ],
        vec![
            "--config".to_owned(),
            "developer_instructions=\"existing\"".to_owned(),
        ],
        vec!["-c=developer_instructions=\"existing\"".to_owned()],
    ] {
        let mut profile_args = vec![
            "-c".to_owned(),
            "features.multi_agent=false".to_owned(),
            "-c".to_owned(),
            "network_access=\"enabled\"".to_owned(),
        ];
        profile_args.extend(developer_args.clone());
        let mut invocation = request(
            "codex",
            ExecAction::Launch {
                prompt: Some("inspect".to_owned()),
                extra_args: profile_args,
            },
        );
        invocation.subagent = true;

        let child = compile_agent_process(project.path(), &invocation, project.path(), None)
            .expect("codex subagent process");
        let matcher = crate::agents::PresetArgMatcher::ConfigKey {
            flags: vec!["-c".to_owned(), "--config".to_owned()],
            key: "developer_instructions".to_owned(),
        };
        let occurrences = matcher.occurrences(&child.provider_argv);

        assert_eq!(occurrences.len(), 1);
        assert_eq!(
            parse_toml_string_or_raw(&occurrences[0].value),
            format!("existing\n\n{}", child_reminder())
        );
        assert!(
            child
                .provider_argv
                .windows(2)
                .any(|args| args == ["-c", "features.multi_agent=false"])
        );
        assert!(
            child
                .provider_argv
                .windows(2)
                .any(|args| args == ["-c", "network_access=\"enabled\""])
        );
        assert_eq!(
            child
                .provider_argv
                .iter()
                .any(|arg| arg.starts_with("-c=developer_instructions=")),
            developer_args.len() == 1
        );
    }
}

#[test]
fn process_compiler_locks_down_opencode_subagent_environment() {
    let project = tempfile::tempdir().expect("project");
    let mut invocation = request(
        "opencode",
        ExecAction::Launch {
            prompt: Some("inspect".to_owned()),
            extra_args: Vec::new(),
        },
    );
    invocation.subagent = true;

    let process = compile_agent_process(project.path(), &invocation, project.path(), None)
        .expect("subagent process");

    // OpenCode's TUI drops arguments after `--`; the task must ride `--prompt`.
    assert!(
        process
            .provider_argv
            .ends_with(&["--prompt".to_owned(), "inspect".to_owned()])
    );
    assert!(!process.provider_argv.iter().any(|arg| arg == "--"));
    assert_eq!(
        process
            .env
            .get("OPENCODE_CONFIG_CONTENT")
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
        Some(
            serde_json::json!({ "permissions": [{ "action": "subagent", "resource": "*", "effect": "deny" }] })
        )
    );
    assert!(
        process
            .provider_argv
            .iter()
            .any(|arg| arg == "--standalone")
    );
}

#[test]
fn launch_environment_precedence_is_project_adapter_then_identity() {
    let adapter = crate::agents::find_definition("copilot").expect("copilot");
    let params = crate::agents::LaunchParams {
        channel: Some("identity".to_owned()),
        ..Default::default()
    };
    let mut invocation = request(
        "copilot",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.identity.params = params;
    let project = env(&[
        (
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT",
            "project",
        ),
        (crate::workspace::ENV_CHANNEL, "project"),
    ]);

    let composed =
        compose_agent_env(project, adapter, &invocation, &BTreeMap::new()).expect("launch env");

    assert_eq!(
        composed["OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"],
        "false"
    );
    assert_eq!(composed[crate::workspace::ENV_CHANNEL], "identity");
}

#[test]
fn process_compiler_names_invalid_environment_key() {
    let adapter = crate::agents::find_definition("codex").expect("codex");
    let invocation = request(
        "codex",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );

    let err = compose_agent_env(
        env(&[("-BROKEN", "value")]),
        adapter,
        &invocation,
        &BTreeMap::new(),
    )
    .expect_err("invalid key");

    assert_eq!(
        err.to_string(),
        "agent `codex` launch env key `-BROKEN` is invalid; environment variable names must be non-empty, cannot contain `=`, and cannot start with `-`"
    );
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn exec_wire_round_trips_maximal_launch_identity() {
    let extra_args = argv(&["--dangerously-skip-permissions"]);
    let params = crate::agents::LaunchParams {
        profile: Some("planner".to_owned()),
        mode: Some(crate::agents::PermissionMode::Yolo),
        role: Some("coder".to_owned()),
        team: Some("forge".to_owned()),
        launch_group: Some("launch_group_1".to_owned()),
        launch_ordinal: Some(2),
        channel: Some("design".to_owned()),
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        budget: Some("$12.50/day".to_owned()),
        kind_ordinal: Some(99),
        ..crate::agents::LaunchParams::default()
    };
    let invocation = ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked("claude"),
        action: ExecAction::Launch {
            prompt: Some("fix it".to_owned()),
            extra_args,
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: Some(vec!["merge".parse().unwrap(), "rebase".parse().unwrap()]),
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: Some(
            "run_0123456789abcdef0123456789abcdef"
                .parse()
                .expect("run id"),
        ),
        worktree_path: Some(PathBuf::from("/repo/worktree")),
        close_pane_on_exit: true,
        exit_on_run_completion: true,
        subagent: true,
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity {
            resume_model_override: false,
            name: Some("swift-otter".to_owned()),
            name_explicit: true,
            launch_id: Some("launch_123".to_owned()),
            params,
        },
    };

    let (argv, decoded) = round_trip(&invocation);
    assert_eq!(
        &argv[..6],
        [
            "/bin/rimz",
            "agents",
            "exec",
            "claude",
            "--worktree-path",
            "/repo/worktree"
        ]
    );
    assert_eq!(argv[6], "--request");
    assert_eq!(decoded.identity.params.kind_ordinal, None);
    let mut expected = invocation;
    expected.identity.params.kind_ordinal = None;
    assert_eq!(decoded, expected);
    for removed in ["--agent-name", "--launch-id", "--prompt", "--agent-profile"] {
        assert!(!argv.iter().any(|arg| arg == removed));
    }
}

#[test]
fn exec_wire_preserves_a_resume_follow_up_message() {
    let request = request(
        "codex",
        ExecAction::Resume {
            session_id: "child".into(),
            extra_args: vec![],
        },
    );
    let mut wire = serde_json::to_value(&request).unwrap();
    assert!(wire.get("follow_up_message").is_none());
    wire["follow_up_message"] = serde_json::to_value(crate::ids::MessageId::new()).unwrap();
    let request: ExecRequest = serde_json::from_value(wire.clone()).unwrap();
    let (_, decoded) = round_trip(&request);
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        wire,
        "the exec wire must retain its follow-up id"
    );
}

#[test]
fn exec_wire_round_trips_resume_and_fork() {
    let extra_args = argv(&["--dangerously-skip-permissions"]);
    let params = crate::agents::LaunchParams {
        profile: Some("planner".to_owned()),
        role: Some("coder".to_owned()),
        team: Some("forge".to_owned()),
        launch_group: Some("launch_group_1".to_owned()),
        launch_ordinal: Some(2),
        channel: Some("design".to_owned()),
        ..Default::default()
    };
    for action in [
        ExecAction::Resume {
            session_id: "resume-1".to_owned(),
            extra_args: extra_args.clone(),
        },
        ExecAction::Fork {
            session_id: "fork-1".to_owned(),
            extra_args: extra_args.clone(),
        },
    ] {
        let mut invocation = request("claude", action);
        invocation.close_pane_on_exit = true;
        invocation.identity = ExecIdentity {
            name: Some("swift-otter".to_owned()),
            params: params.clone(),
            ..ExecIdentity::default()
        };
        let (argv, decoded) = round_trip(&invocation);
        assert_eq!(argv[..4], ["/bin/rimz", "agents", "exec", "claude"]);
        let wire: serde_json::Value =
            serde_json::from_str(argv.last().expect("payload")).expect("wire");
        assert!(wire.get("prompt_file").is_none());
        assert_eq!(decoded, invocation);
    }
}

#[test]
fn exec_wire_defaults_missing_skills_and_rejects_duplicates() {
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    let mut payload = serde_json::to_value(&request).unwrap();
    assert!(payload.get("skills").is_none());
    assert!(
        decode_exec_request("claude", None, &payload.to_string())
            .unwrap()
            .skills
            .is_none()
    );
    payload["skills"] = serde_json::json!([]);
    let empty = decode_exec_request("claude", None, &payload.to_string()).unwrap();
    assert_eq!(empty.skills, Some(Vec::new()));
    assert_eq!(
        serde_json::to_value(&empty).unwrap()["skills"],
        serde_json::json!([])
    );
    payload["skills"] = serde_json::json!(["merge", "merge"]);
    assert!(
        decode_exec_request("claude", None, &payload.to_string())
            .unwrap_err()
            .to_string()
            .contains("duplicate skill name `merge`")
    );
    request.skills = Some(vec!["merge".parse().unwrap(), "merge".parse().unwrap()]);
    assert!(matches!(
        exec_argv(
            Path::new("/bin/rimz"),
            &runtime(Path::new("/tmp/unused")),
            &request
        ),
        Err(ExecWireErr::Skills(
            crate::config::SkillListErr::DuplicateName(_)
        ))
    ));
}

#[test]
fn allowed_tools_exec_wire_defaults_and_revalidates() {
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    let mut payload = serde_json::to_value(&request).unwrap();
    assert!(payload.get("allowed_tools").is_none());
    assert!(
        decode_exec_request("claude", None, &payload.to_string())
            .unwrap()
            .allowed_tools
            .is_none()
    );
    payload["allowed_tools"] = serde_json::json!(["Bash(git *"]);
    assert!(decode_exec_request("claude", None, &payload.to_string()).is_err());
    for rules in [
        serde_json::json!([]),
        serde_json::json!(["Bash(git *)", "Read"]),
    ] {
        payload["allowed_tools"] = rules.clone();
        let decoded = decode_exec_request("claude", None, &payload.to_string()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["allowed_tools"],
            rules
        );
    }
}

#[test]
fn exec_identity_env_maps_identity_fields() {
    let params = crate::agents::LaunchParams {
        profile: Some("planner".to_owned()),
        role: Some("coder".to_owned()),
        team: Some("forge".to_owned()),
        launch_group: Some("launch_group_1".to_owned()),
        launch_ordinal: Some(2),
        channel: Some("design".to_owned()),
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        budget: Some("$12.50/day".to_owned()),
        ..Default::default()
    };
    let mut invocation = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.run_id = Some(
        "run_0123456789abcdef0123456789abcdef"
            .parse()
            .expect("run id"),
    );
    invocation.identity = ExecIdentity {
        name: Some("swift-otter".to_owned()),
        launch_id: Some("launch_swift_otter".to_owned()),
        params,
        ..ExecIdentity::default()
    };

    assert_eq!(
        exec_identity_env(&invocation),
        BTreeMap::from([
            (
                crate::harness::launch::ENV_AGENT_KIND.to_owned(),
                "claude".to_owned()
            ),
            (
                crate::harness::launch::ENV_AGENT_ID.to_owned(),
                "launch_swift_otter".to_owned()
            ),
            (
                crate::harness::launch::ENV_RUN_ID.to_owned(),
                "run_0123456789abcdef0123456789abcdef".to_owned()
            ),
            (
                crate::harness::launch::ENV_AGENT_NAME.to_owned(),
                "swift-otter".to_owned(),
            ),
            (
                crate::harness::launch::ENV_AGENT_PROFILE.to_owned(),
                "planner".to_owned(),
            ),
            (
                crate::harness::launch::ENV_AGENT_ROLE.to_owned(),
                "coder".to_owned()
            ),
            (
                crate::harness::launch::ENV_TEAM.to_owned(),
                "forge".to_owned()
            ),
            (
                crate::harness::launch::ENV_LAUNCH_GROUP.to_owned(),
                "launch_group_1".to_owned(),
            ),
            (
                crate::harness::launch::ENV_LAUNCH_ORDINAL.to_owned(),
                "2".to_owned()
            ),
            (
                crate::workspace::ENV_CHANNEL.to_owned(),
                "design".to_owned()
            ),
            (
                crate::harness::launch::ENV_AGENT_MODEL.to_owned(),
                "opus".to_owned()
            ),
            (
                crate::harness::launch::ENV_AGENT_EFFORT.to_owned(),
                "high".to_owned()
            ),
            (
                crate::harness::launch::ENV_AGENT_BUDGET.to_owned(),
                "$12.50/day".to_owned()
            ),
        ])
    );
    assert!(!exec_identity_env(&invocation).contains_key("RIMZ_AGENT_MODE"));

    invocation.identity.launch_id = None;
    assert_eq!(
        exec_identity_env(&invocation)
            .get(crate::harness::launch::ENV_AGENT_ID)
            .map(String::as_str),
        Some(""),
        "missing ids overwrite rather than inherit an ambient caller id"
    );
}

#[test]
fn exec_identity_env_round_trips_launch_params() {
    let params = crate::agents::LaunchParams {
        profile: Some("planner".to_owned()),
        role: Some("coder".to_owned()),
        team: Some("forge".to_owned()),
        launch_group: Some("launch_group_1".to_owned()),
        launch_ordinal: Some(2),
        channel: Some("design".to_owned()),
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        budget: Some("$12.50/day".to_owned()),
        ..Default::default()
    };
    let mut invocation = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.identity.params = params.clone();
    let env = exec_identity_env(&invocation);

    let mut decoded = crate::agents::LaunchParams::default();
    let mut read = Vec::new();
    fill_launch_identity_env(&mut decoded, |var| {
        read.push(var);
        env.get(var).cloned()
    });
    fill_launch_identity_env(&mut decoded, |var| {
        panic!("populated field {var} must not be re-read")
    });

    assert_eq!(decoded, params);
    read.sort_unstable();
    let mut expected = vec![
        ENV_AGENT_ROLE,
        ENV_TEAM,
        ENV_LAUNCH_GROUP,
        ENV_LAUNCH_ORDINAL,
        crate::workspace::ENV_CHANNEL,
        ENV_AGENT_PROFILE,
        ENV_AGENT_MODEL,
        ENV_AGENT_EFFORT,
        ENV_AGENT_BUDGET,
    ];
    expected.sort_unstable();
    assert_eq!(read, expected);
}

#[test]
fn exec_wire_rejects_launch_id_without_a_name() {
    let mut invocation = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.identity.launch_id = Some("launch_orphan".to_owned());

    assert_eq!(
        exec_argv(
            Path::new("/bin/rimz"),
            &runtime(Path::new("/tmp/unused")),
            &invocation
        )
        .expect_err("orphan launch id")
        .to_string(),
        "--launch-id requires --agent-name"
    );
}

fn provider_binding(key: &str) -> crate::agents::ProviderAccountBinding {
    crate::agents::ProviderAccountBinding::decode(&format!(
        r#"{{"scope":{{"kind":"sub_provider","provider":"alibaba","variant":"international"}},"account_key":"{key}"}}"#
    ))
    .expect("provider binding")
}

#[test]
fn exec_wire_rejects_malformed_and_mismatched_envelopes() {
    let mut missing_run = request(
        "codex",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    missing_run.exit_on_run_completion = true;
    assert_eq!(
        exec_argv(
            Path::new("/bin/rimz"),
            &runtime(Path::new("/tmp/unused")),
            &missing_run
        )
        .expect_err("run id required")
        .to_string(),
        "--exit-on-run-completion requires --run-id"
    );

    let input = request(
        "codex",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    let argv = exec_argv(
        Path::new("/bin/rimz"),
        &runtime(Path::new("/tmp/unused")),
        &input,
    )
    .expect("argv");
    let payload = argv.last().expect("payload");
    assert!(matches!(
        decode_exec_request("claude", None, payload),
        Err(ExecWireErr::KindMismatch { .. })
    ));
    assert!(matches!(
        decode_exec_request("codex", Some(Path::new("/other")), payload),
        Err(ExecWireErr::WorktreeMismatch)
    ));

    let mut value = serde_json::to_value(&input).expect("request value");
    value
        .as_object_mut()
        .expect("request serializes as an object")
        .remove("subagent");
    let decoded =
        decode_exec_request("codex", None, &value.to_string()).expect("legacy request payload");
    assert!(!decoded.subagent);

    value["action"]["prompt"] = serde_json::json!("legacy inline\nprompt");
    let decoded = decode_exec_request("codex", None, &value.to_string()).expect("inline prompt");
    assert!(
        matches!(decoded.action, ExecAction::Launch { prompt: Some(ref prompt), .. } if prompt == "legacy inline\nprompt")
    );

    value["provider_account"] = serde_json::json!({ "state": "finalized" });
    let err = decode_exec_request("codex", None, &value.to_string())
        .expect_err("finalized binding required");
    assert!(
        err.to_string()
            .contains("finalized provider-account launch is missing its expected binding"),
        "{err}"
    );
}

#[test]
fn provider_account_stage_validates_and_reenters_once() {
    let project = tempfile::tempdir().expect("project");
    let runtime = runtime(project.path());
    let binding = provider_binding("owner");
    for (kind, action) in [
        (
            "codex",
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        ),
        (
            "qwen",
            ExecAction::Resume {
                session_id: "sess-1".to_owned(),
                extra_args: Vec::new(),
            },
        ),
        (
            "qwen",
            ExecAction::Fork {
                session_id: "sess-1".to_owned(),
                extra_args: Vec::new(),
            },
        ),
    ] {
        let mut input = request(kind, action);
        input.provider_account = ProviderAccountState::Pending {
            binding: binding.clone(),
        };
        let err = compile_agent_process_stage_with_extra_env(
            project.path(),
            &input,
            project.path(),
            Path::new("/bin/rimz"),
            &runtime,
            &BTreeMap::new(),
            &LaunchReminders::default(),
        )
        .expect_err("binding scope");
        assert_eq!(
            err.to_string(),
            "provider-account binding applies only to fresh managed Qwen launches"
        );
    }

    let mut pending = request(
        "qwen",
        ExecAction::Launch {
            prompt: Some("reentry prompt".to_owned()),
            extra_args: Vec::new(),
        },
    );
    pending.provider_account = ProviderAccountState::Pending {
        binding: binding.clone(),
    };
    let stage = compile_agent_process_stage_with_extra_env(
        project.path(),
        &pending,
        project.path(),
        Path::new("/bin/rimz"),
        &runtime,
        &BTreeMap::new(),
        &LaunchReminders::default(),
    )
    .expect("pending stage");
    let AgentProcessStage::LoginShellReentry {
        argv,
        prompt_artifacts,
        ..
    } = stage
    else {
        panic!("pending binding must re-enter");
    };
    let artifact = prompt_artifacts
        .into_iter()
        .next()
        .expect("reentry prompt artifact");
    assert!(!artifact.path.exists());
    write_prompt_artifact(&artifact).expect("apply reentry prompt artifact");
    let payload = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
        .expect("reentry payload");
    let finalized = decode_exec_request("qwen", None, payload).expect("finalized request");
    assert_eq!(finalized.action, pending.action);
    let initial = exec_argv(Path::new("/bin/rimz"), &runtime, &pending).expect("initial argv");
    let initial: ExecWire = serde_json::from_str(initial.last().expect("payload")).expect("wire");
    let reentry: ExecWire = serde_json::from_str(payload).expect("wire");
    assert_eq!(initial.prompt_file, reentry.prompt_file);
    assert!(matches!(
        finalized.provider_account,
        ProviderAccountState::Finalized { .. }
    ));

    let err = compile_agent_process_stage_with_extra_env(
        project.path(),
        &finalized,
        project.path(),
        Path::new("/bin/rimz"),
        &runtime,
        &BTreeMap::new(),
        &LaunchReminders::default(),
    )
    .expect_err("unresolved account mismatches");
    assert!(matches!(
        err,
        AgentProcessStageErr::FinalizedProviderMismatch
    ));
    assert!(!format!("{err:?}").contains("owner"));

    let unbound = request(
        "codex",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    let expected = compile_agent_process(project.path(), &unbound, project.path(), None)
        .expect("ordinary process");
    let AgentProcessStage::Ready(ordinary) = compile_agent_process_stage_with_extra_env(
        project.path(),
        &unbound,
        project.path(),
        Path::new("/bin/rimz"),
        &runtime,
        &BTreeMap::new(),
        &LaunchReminders::default(),
    )
    .expect("ordinary stage") else {
        panic!("unbound launch is ready");
    };
    assert_eq!(ordinary.argv, expected.argv);
    assert_ne!(ordinary.argv, ordinary.provider_argv);

    let mut finalized_request = request(
        "qwen",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    finalized_request.provider_account = ProviderAccountState::Finalized {
        binding: binding.clone(),
    };
    let process = compile_agent_process(project.path(), &finalized_request, project.path(), None)
        .expect("finalized process");
    let raw = process.provider_argv.clone();
    let AgentProcessStage::Ready(mut finalized) = finalize_agent_process_stage(
        process,
        &finalized_request,
        Some(&crate::agents::ManagedLaunchState::Bound(binding)),
        Path::new("/bin/rimz"),
        &runtime,
    )
    .expect("matching finalized binding") else {
        panic!("finalized launch is ready");
    };
    assert_eq!(finalized.argv, raw);
    assert!(finalized.provider_argv.is_empty());
    finalized.pin_env(BTreeMap::from([
        (
            "TMPDIR".to_owned(),
            crate::sandbox::EnvPin::Set("/tmp".to_owned()),
        ),
        (
            "CLAUDE_CONFIG_DIR".to_owned(),
            crate::sandbox::EnvPin::Unset,
        ),
    ]));
    assert_eq!(
        finalized.argv, raw,
        "post-rc stages do not re-enter a shell"
    );
    assert_eq!(finalized.env["TMPDIR"], "/tmp");
    assert!(finalized.unset.contains("CLAUDE_CONFIG_DIR"));
}

#[test]
fn shell_family_matches_known_basenames() {
    assert_eq!(
        ShellFamily::from_shell(Path::new("/bin/bash")),
        ShellFamily::Bash
    );
    assert_eq!(
        ShellFamily::from_shell(Path::new("/usr/bin/zsh")),
        ShellFamily::Posix
    );
    assert_eq!(
        ShellFamily::from_shell(Path::new("/usr/bin/fish")),
        ShellFamily::Fish
    );
    assert_eq!(
        ShellFamily::from_shell(Path::new("/bin/tcsh")),
        ShellFamily::Csh
    );
    assert_eq!(
        ShellFamily::from_shell(Path::new("/opt/bin/custom")),
        ShellFamily::Posix
    );
}

#[test]
fn bash_wrapper_uses_interactive_rc_shape() {
    let wrapped = login_shell_argv_with(
        Some(Path::new("/bin/bash")),
        true,
        &env(&[("AAA", "one")]),
        &BTreeSet::new(),
        &argv(&["codex"]),
    );

    assert_eq!(
        wrapped,
        vec![
            "/bin/bash",
            "-i",
            "-c",
            POSIX_LOGIN_SHELL_SCRIPT,
            POSIX_ARG0,
            "AAA=one",
            "codex",
        ]
    );
}

#[test]
fn posix_wrapper_shape_reapplies_env_after_rc() {
    let wrapped = login_shell_argv_with(
        Some(Path::new("/bin/sh")),
        true,
        &env(&[("AAA", "one"), ("BBB", "two")]),
        &BTreeSet::from(["CLAUDE_CONFIG_DIR".to_owned()]),
        &argv(&["codex", "prompt with spaces"]),
    );

    assert_eq!(
        wrapped,
        vec![
            "/bin/sh",
            "-l",
            "-i",
            "-c",
            POSIX_LOGIN_SHELL_SCRIPT,
            POSIX_ARG0,
            "-u",
            "CLAUDE_CONFIG_DIR",
            "AAA=one",
            "BBB=two",
            "codex",
            "prompt with spaces",
        ]
    );
}

#[test]
fn compiled_process_debug_prints_launch_env_keys_without_values() {
    let launch_env = env(&[
        ("ANTHROPIC_API_KEY", "sk-secret"),
        ("VISIBLE_SETTING", "auto"),
    ]);
    let provider_argv = argv(&["claude", "--print", "--setting=visible"]);
    let wrapped = login_shell_argv_with(
        Some(Path::new("/bin/sh")),
        true,
        &launch_env,
        &BTreeSet::new(),
        &provider_argv,
    );
    let process = CompiledAgentProcess {
        host_skills: None,
        settings_artifact: None,
        provider_argv,
        provider_program: "claude".to_owned(),
        argv: wrapped.clone(),
        env: launch_env,
        secret_keys: BTreeSet::from(["ANTHROPIC_API_KEY".to_owned()]),
        reminder: String::new(),
        unset: BTreeSet::new(),
        shell: Some(PathBuf::from("/bin/sh")),
    };

    let report_argv = redact_env_tokens(&wrapped, |key| process.secret_keys.contains(key));
    assert!(report_argv.contains(&"ANTHROPIC_API_KEY=<redacted>".to_owned()));
    assert!(report_argv.contains(&"VISIBLE_SETTING=auto".to_owned()));
    assert!(report_argv.contains(&"--setting=visible".to_owned()));

    let rendered = format!("{process:?}");
    assert!(!rendered.contains("sk-secret"), "{rendered}");
    assert!(rendered.contains("ANTHROPIC_API_KEY"), "{rendered}");
    assert!(rendered.contains("--print"), "{rendered}");
    assert!(rendered.contains("--setting=visible"), "{rendered}");
    assert!(!rendered.contains("VISIBLE_SETTING=auto"), "{rendered}");

    let rendered = format!(
        "{:?}",
        AgentProcessStage::LoginShellReentry {
            process,
            argv: wrapped,
            prompt_artifacts: Vec::new(),
        }
    );
    assert!(!rendered.contains("sk-secret"), "{rendered}");
    assert!(rendered.contains("ANTHROPIC_API_KEY"), "{rendered}");
}

#[test]
fn fish_wrapper_uses_argv_without_a_posix_arg0() {
    let wrapped = login_shell_argv_with(
        Some(Path::new("/usr/bin/fish")),
        true,
        &env(&[("AAA", "one")]),
        &BTreeSet::from(["CODEX_HOME".to_owned()]),
        &argv(&["claude"]),
    );

    assert_eq!(
        wrapped,
        vec![
            "/usr/bin/fish",
            "-l",
            "-i",
            "-c",
            FISH_LOGIN_SHELL_SCRIPT,
            "-u",
            "CODEX_HOME",
            "AAA=one",
            "claude",
        ]
    );
}

#[test]
fn unsupported_or_unavailable_wrapper_falls_back_to_agent_argv() {
    let command = argv(&["codex"]);
    let launch_env = env(&[("AAA", "one")]);

    assert_eq!(
        login_shell_argv_with(None, true, &launch_env, &BTreeSet::new(), &command),
        command
    );
    assert_eq!(
        login_shell_argv_with(
            Some(Path::new("/bin/sh")),
            false,
            &launch_env,
            &BTreeSet::new(),
            &command
        ),
        command
    );
    assert_eq!(
        login_shell_argv_with(
            Some(Path::new("/bin/tcsh")),
            true,
            &launch_env,
            &BTreeSet::new(),
            &command
        ),
        command
    );
    assert_eq!(
        login_shell_argv_with(
            Some(Path::new("/bin/sh")),
            true,
            &env(&[("BAD=KEY", "one")]),
            &BTreeSet::new(),
            &command,
        ),
        command
    );
    assert_eq!(
        login_shell_argv_with(
            Some(Path::new("/bin/sh")),
            true,
            &env(&[("-BAD", "one")]),
            &BTreeSet::new(),
            &command,
        ),
        command
    );
}

#[test]
fn invalid_env_key_reports_the_first_unrepresentable_key() {
    assert_eq!(
        invalid_env_key(&env(&[("BAD=KEY", "one")])),
        Some("BAD=KEY")
    );
    assert_eq!(invalid_env_key(&env(&[("", "one")])), Some(""));
    assert_eq!(invalid_env_key(&env(&[("-BAD", "one")])), Some("-BAD"));
    assert_eq!(invalid_env_key(&env(&[("GOOD_KEY", "one")])), None);
}

#[test]
fn program_lookup_uses_path_from_shell_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    let agent = bin_dir.join(unique_probe_program());
    std::fs::write(&agent, "#!/bin/sh\nexit 0\n").expect("write agent");
    chmod_executable(&agent);

    let shell = dir.path().join("bash");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\n\
                 export PATH='{}':\"$PATH\"\n\
                 while [ \"$#\" -gt 0 ]; do\n\
                   case \"$1\" in\n\
                     -c)\n\
                       shift\n\
                       script=$1\n\
                       shift\n\
                       exec /bin/sh -c \"$script\" \"$@\"\n\
                       ;;\n\
                     *) shift ;;\n\
                   esac\n\
                 done\n\
                 exit 127\n",
            bin_dir.display()
        ),
    )
    .expect("write shell");
    chmod_executable(&shell);

    assert_eq!(
        resolve_program_with(
            Some(&shell),
            true,
            &BTreeMap::new(),
            agent.file_name().unwrap().to_str().unwrap()
        )
        .expect("lookup"),
        Some(agent)
    );
}

#[test]
fn program_lookup_rejects_invalid_launch_env_keys() {
    let err = resolve_program_with(
        Some(Path::new("/bin/sh")),
        true,
        &env(&[("BAD=KEY", "one")]),
        "codex",
    )
    .expect_err("invalid key");

    assert!(err.to_string().contains("BAD=KEY"));
}

#[test]
fn probe_timeout_names_the_shell_that_ran() {
    let err = ProgramLookupErr::ProbeTimeout {
        program: "/bin/zsh".to_owned(),
        timeout: ENV_PROBE_TIMEOUT,
    };

    assert_eq!(
        err.to_string(),
        "launch environment probe `/bin/zsh` timed out after 5s"
    );
}

fn unique_probe_program() -> String {
    format!("rimz-agent-probe-{}", std::process::id())
}

fn chmod_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

#[test]
fn the_room_account_home_wins_over_a_trusted_project_env() {
    let adapter = crate::agents::find_definition("claude").expect("claude");
    let invocation = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    let project = env(&[("CLAUDE_CONFIG_DIR", "/project/home")]);
    let login = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("claude"),
        "work".parse().expect("login name"),
        std::path::PathBuf::from("/srv/work"),
    )
    .expect("named login");

    let composed = compose_agent_env(project, adapter, &invocation, &login.env(&BTreeMap::new()))
        .expect("launch env");

    assert_eq!(composed["CLAUDE_CONFIG_DIR"], "/srv/work");
}

fn shell_file(dir: &Path, name: &str) -> PathBuf {
    let shell = dir.join(name);
    std::fs::write(&shell, "").expect("write shell");
    shell
}

fn compile_under_agent_shell(
    project: &Path,
    agent_shell: Option<PathBuf>,
) -> AgentProcessResult<CompiledAgentProcess> {
    compile_agent_process_with_extra_env(
        None,
        project,
        &request(
            "claude",
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        ),
        project,
        &BTreeMap::new(),
        &LaunchReminders {
            env: true,
            agent_shell,
            ..LaunchReminders::default()
        },
    )
}

#[test]
fn agent_shell_validation_refuses_what_the_wrapper_cannot_run_under() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (shell, reason) in [
        (PathBuf::from("bash"), "is not an absolute path"),
        (dir.path().join("zsh"), "does not exist"),
        (shell_file(dir.path(), "nologin"), "disables logins"),
        (shell_file(dir.path(), "tcsh"), "is a csh-family shell"),
    ] {
        let err = validate_agent_shell(&shell).expect_err("refused shell");
        assert_eq!(
            err.to_string(),
            format!(
                "[agents] shell = \"{}\" {reason}; set it to the absolute path of an installed bash, zsh, fish or sh-family shell, or remove the line from the per-machine config",
                shell.display()
            )
        );
        let blocker = crate::blocker::Blocker::from(err);
        assert!(
            blocker.fixes.iter().any(|fix| {
                fix.command.as_deref()
                    == Some("rimz config set agents.shell \"$(command -v bash)\"")
                    && fix.surface == crate::blocker::Surface::Plain
            }),
            "{blocker:?}"
        );
    }
    for name in ["bash", "fish"] {
        validate_agent_shell(&shell_file(dir.path(), name)).expect("accepted shell");
    }
}

#[test]
fn configured_agent_shell_carries_the_wrapper_env_and_reminder() {
    let project = tempfile::tempdir().expect("project");
    let bash = shell_file(project.path(), "bash");

    let process =
        compile_under_agent_shell(project.path(), Some(bash.clone())).expect("compiled process");

    let configured = bash.display().to_string();
    assert_eq!(process.env.get("SHELL"), Some(&configured));
    assert_eq!(process.env.get(ENV_CLAUDE_CODE_SHELL), Some(&configured));
    assert_eq!(
        process.env.get(crate::proc::USER_SHELL_ENV),
        crate::proc::user_shell()
            .map(|shell| shell.display().to_string())
            .as_ref()
    );
    assert_eq!(process.argv.first(), Some(&configured));
    assert!(process.reminder.contains("- shell: bash\n"));

    let mut pinned = process.clone();
    pinned.pin_env(BTreeMap::from([(
        "HOME".to_owned(),
        crate::sandbox::EnvPin::Set("/sandbox/home".to_owned()),
    )]));
    assert_eq!(pinned.argv.first(), Some(&configured));

    let missing = project.path().join("missing").join("bash");
    let err = compile_under_agent_shell(project.path(), Some(missing)).expect_err("refused");
    assert!(err.to_string().contains("[agents] shell"), "{err}");
}

#[test]
fn unset_agent_shell_adds_no_shell_env() {
    let project = tempfile::tempdir().expect("project");

    let process = compile_under_agent_shell(project.path(), None).expect("compiled process");

    for key in ["SHELL", ENV_CLAUDE_CODE_SHELL, crate::proc::USER_SHELL_ENV] {
        assert!(!process.env.contains_key(key), "{key}");
    }
}

#[test]
fn agent_shell_env_beats_a_trusted_project_shell() {
    let adapter = crate::agents::find_definition("claude").expect("claude");
    let invocation = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let bash = shell_file(dir.path(), "bash");

    let composed = compose_agent_env(
        env(&[
            ("SHELL", "/project/zsh"),
            (ENV_CLAUDE_CODE_SHELL, "/project/zsh"),
        ]),
        adapter,
        &invocation,
        &agent_shell_env(&bash, Some(Path::new("/bin/zsh"))),
    )
    .expect("launch env");

    assert_eq!(composed["SHELL"], bash.display().to_string());
    assert_eq!(composed[ENV_CLAUDE_CODE_SHELL], bash.display().to_string());
    assert_eq!(composed[crate::proc::USER_SHELL_ENV], "/bin/zsh");
}

#[test]
fn program_lookup_runs_under_the_configured_agent_shell() {
    let project = tempfile::tempdir().expect("project");
    let bin_dir = project.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    let agent = bin_dir.join(format!("{}-agent-shell", unique_probe_program()));
    std::fs::write(&agent, "#!/bin/sh\nexit 0\n").expect("write agent");
    chmod_executable(&agent);
    let shell = project.path().join("bash");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\n\
             export PATH='{}':\"$PATH\"\n\
             while [ \"$#\" -gt 0 ]; do\n\
               case \"$1\" in\n\
                 -c) shift; script=$1; shift; exec /bin/sh -c \"$script\" \"$@\" ;;\n\
                 *) shift ;;\n\
               esac\n\
             done\n\
             exit 127\n",
            bin_dir.display()
        ),
    )
    .expect("write shell");
    chmod_executable(&shell);

    let mut process =
        compile_under_agent_shell(project.path(), Some(shell)).expect("compiled process");
    process.provider_program = agent.file_name().unwrap().to_str().unwrap().to_owned();

    assert_eq!(
        process.resolve_program_after_shell_rc().expect("lookup"),
        Some(agent)
    );
}
