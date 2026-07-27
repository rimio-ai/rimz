use super::*;

fn env(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
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
    }
}

fn request(kind: &str, action: ExecAction) -> ExecRequest {
    ExecRequest {
        kind: AgentKind::new_unchecked(kind),
        action,
        system_prompt: Default::default(),
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: ExecIdentity::default(),
    }
}

fn round_trip(input: &ExecRequest) -> (Vec<String>, ExecRequest) {
    let argv = exec_argv(Path::new("/bin/rimz"), input).expect("encode exec request");
    let payload = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
        .expect("request payload");
    let decoded = decode_exec_request(input.kind.as_str(), input.worktree_path.as_deref(), payload)
        .expect("decode exec request");
    (argv, decoded)
}

#[test]
fn process_compiler_composes_adapter_identity_and_rtk_environment() {
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

    let process = compile_agent_process(
        project.path(),
        crate::config::RtkMode::On,
        &invocation,
        project.path(),
    )
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
            .get(crate::harness::run::ENV_AGENT_KIND)
            .map(String::as_str),
        Some("copilot")
    );
    assert_eq!(
        process
            .env
            .get(crate::harness::run::ENV_CHANNEL)
            .map(String::as_str),
        Some("design")
    );
    assert_eq!(
        process
            .env
            .get(crate::harness::run::ENV_RTK)
            .map(String::as_str),
        Some("on")
    );
}

#[test]
fn launch_environment_precedence_is_project_adapter_identity_then_rtk() {
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
        (crate::harness::run::ENV_CHANNEL, "project"),
        (crate::harness::run::ENV_RTK, "project"),
    ]);

    let composed = compose_agent_env(project, adapter, crate::config::RtkMode::Off, &invocation)
        .expect("launch env");

    assert_eq!(
        composed["OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"],
        "false"
    );
    assert_eq!(composed[crate::harness::run::ENV_CHANNEL], "identity");
    assert_eq!(composed[crate::harness::run::ENV_RTK], "off");
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
        crate::config::RtkMode::Auto,
        &invocation,
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
        mode: Some(crate::harness::run::PermissionMode::Yolo),
        role: Some("coder".to_owned()),
        team: Some("forge".to_owned()),
        launch_group: Some("launch_group_1".to_owned()),
        launch_ordinal: Some(2),
        channel: Some("design".to_owned()),
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        budget: Some("$12.50/day".to_owned()),
        kind_ordinal: Some(99),
    };
    let invocation = ExecRequest {
        kind: AgentKind::new_unchecked("claude"),
        action: ExecAction::Launch {
            prompt: Some("fix it".to_owned()),
            extra_args,
        },
        system_prompt: Default::default(),
        provider_account: ProviderAccountState::Unbound,
        run_id: Some(
            "run_0123456789abcdef0123456789abcdef"
                .parse()
                .expect("run id"),
        ),
        worktree_path: Some(PathBuf::from("/repo/worktree")),
        close_pane_on_exit: true,
        exit_on_run_completion: true,
        identity: ExecIdentity {
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
        assert_eq!(decoded, invocation);
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
        params,
        ..ExecIdentity::default()
    };

    assert_eq!(
        exec_identity_env(&invocation),
        BTreeMap::from([
            (
                crate::harness::run::ENV_AGENT_KIND.to_owned(),
                "claude".to_owned()
            ),
            (
                crate::harness::run::ENV_RUN_ID.to_owned(),
                "run_0123456789abcdef0123456789abcdef".to_owned()
            ),
            (
                crate::harness::run::ENV_AGENT_NAME.to_owned(),
                "swift-otter".to_owned(),
            ),
            (
                crate::harness::run::ENV_AGENT_PROFILE.to_owned(),
                "planner".to_owned(),
            ),
            (
                crate::harness::run::ENV_AGENT_ROLE.to_owned(),
                "coder".to_owned()
            ),
            (crate::harness::run::ENV_TEAM.to_owned(), "forge".to_owned()),
            (
                crate::harness::run::ENV_LAUNCH_GROUP.to_owned(),
                "launch_group_1".to_owned(),
            ),
            (
                crate::harness::run::ENV_LAUNCH_ORDINAL.to_owned(),
                "2".to_owned()
            ),
            (
                crate::harness::run::ENV_CHANNEL.to_owned(),
                "design".to_owned()
            ),
            (
                crate::harness::run::ENV_AGENT_MODEL.to_owned(),
                "opus".to_owned()
            ),
            (
                crate::harness::run::ENV_AGENT_EFFORT.to_owned(),
                "high".to_owned()
            ),
            (
                crate::harness::run::ENV_AGENT_BUDGET.to_owned(),
                "$12.50/day".to_owned()
            ),
        ])
    );
    assert!(!exec_identity_env(&invocation).contains_key("RIMZ_AGENT_MODE"));
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
        exec_argv(Path::new("/bin/rimz"), &invocation)
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
        exec_argv(Path::new("/bin/rimz"), &missing_run)
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
    let argv = exec_argv(Path::new("/bin/rimz"), &input).expect("argv");
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
        let err = compile_agent_process_stage(
            project.path(),
            crate::config::RtkMode::Auto,
            &input,
            project.path(),
            Path::new("/bin/rimz"),
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
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    pending.provider_account = ProviderAccountState::Pending {
        binding: binding.clone(),
    };
    let stage = compile_agent_process_stage(
        project.path(),
        crate::config::RtkMode::Auto,
        &pending,
        project.path(),
        Path::new("/bin/rimz"),
    )
    .expect("pending stage");
    let AgentProcessStage::LoginShellReentry { argv, .. } = stage else {
        panic!("pending binding must re-enter");
    };
    let payload = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
        .expect("reentry payload");
    let finalized = decode_exec_request("qwen", None, payload).expect("finalized request");
    assert!(matches!(
        finalized.provider_account,
        ProviderAccountState::Finalized { .. }
    ));

    let err = compile_agent_process_stage(
        project.path(),
        crate::config::RtkMode::Auto,
        &finalized,
        project.path(),
        Path::new("/bin/rimz"),
    )
    .expect_err("unresolved account mismatches");
    assert!(err.is_finalized_provider_mismatch());
    assert!(!format!("{err:?}").contains("owner"));

    let unbound = request(
        "codex",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    let expected = compile_agent_process(
        project.path(),
        crate::config::RtkMode::Auto,
        &unbound,
        project.path(),
    )
    .expect("ordinary process");
    let AgentProcessStage::Ready(ordinary) = compile_agent_process_stage(
        project.path(),
        crate::config::RtkMode::Auto,
        &unbound,
        project.path(),
        Path::new("/bin/rimz"),
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
    let process = compile_agent_process(
        project.path(),
        crate::config::RtkMode::Auto,
        &finalized_request,
        project.path(),
    )
    .expect("finalized process");
    let raw = process.provider_argv.clone();
    let AgentProcessStage::Ready(finalized) = finalize_agent_process_stage(
        process,
        &finalized_request,
        Some(&crate::agents::ManagedLaunchState::Bound(binding)),
        Path::new("/bin/rimz"),
    )
    .expect("matching finalized binding") else {
        panic!("finalized launch is ready");
    };
    assert_eq!(finalized.argv, raw);
    assert!(finalized.provider_argv.is_empty());
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
            "AAA=one",
            "BBB=two",
            "codex",
            "prompt with spaces",
        ]
    );
}

#[test]
fn fish_wrapper_uses_argv_without_a_posix_arg0() {
    let wrapped = login_shell_argv_with(
        Some(Path::new("/usr/bin/fish")),
        true,
        &env(&[("AAA", "one")]),
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
        login_shell_argv_with(None, true, &launch_env, &command),
        command
    );
    assert_eq!(
        login_shell_argv_with(Some(Path::new("/bin/sh")), false, &launch_env, &command),
        command
    );
    assert_eq!(
        login_shell_argv_with(Some(Path::new("/bin/tcsh")), true, &launch_env, &command),
        command
    );
    assert_eq!(
        login_shell_argv_with(
            Some(Path::new("/bin/sh")),
            true,
            &env(&[("BAD=KEY", "one")]),
            &command,
        ),
        command
    );
    assert_eq!(
        login_shell_argv_with(
            Some(Path::new("/bin/sh")),
            true,
            &env(&[("-BAD", "one")]),
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

    assert!(
        program_resolves_with(
            Some(&shell),
            true,
            &BTreeMap::new(),
            agent.file_name().unwrap().to_str().unwrap()
        )
        .expect("lookup")
    );
}

#[test]
fn program_lookup_rejects_invalid_launch_env_keys() {
    let err = program_resolves_with(
        Some(Path::new("/bin/sh")),
        true,
        &env(&[("BAD=KEY", "one")]),
        "codex",
    )
    .expect_err("invalid key");

    assert!(err.to_string().contains("BAD=KEY"));
}

#[test]
fn launchable_shell_rejects_missing_and_disabled_shells() {
    assert!(!launchable_shell(Path::new("/definitely/not/a/shell")));
    assert!(!launchable_shell(Path::new("/usr/sbin/nologin")));
    assert!(!launchable_shell(Path::new("/bin/false")));
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
