use super::*;

fn env(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn provider_compiler_preserves_action_and_trailing_argument_order() {
    let adapter = crate::agents::find_adapter("codex").expect("codex adapter");
    let trailing = vec!["--model".to_owned(), "o3".to_owned()];
    for (action, verb, session) in [
        (
            ExecAction::Launch {
                prompt: Some("inspect"),
                extra_args: &trailing,
            },
            "inspect",
            None,
        ),
        (
            ExecAction::Fork {
                session_id: "fork-id",
                extra_args: &trailing,
            },
            "fork",
            Some("fork-id"),
        ),
        (
            ExecAction::Resume {
                session_id: "resume-id",
                extra_args: &trailing,
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

#[test]
fn process_compiler_composes_adapter_identity_and_rtk_environment() {
    let project = tempfile::tempdir().expect("project");
    let invocation = ExecInvocation {
        kind: "copilot",
        action: ExecAction::Launch {
            prompt: Some("inspect"),
            extra_args: &[],
        },
        run_id: Some("run_123"),
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: ExecIdentity {
            channel: Some("design"),
            ..ExecIdentity::default()
        },
    };

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
    let adapter = crate::agents::find_adapter("copilot").expect("copilot");
    let invocation = ExecInvocation {
        kind: "copilot",
        action: ExecAction::Launch {
            prompt: None,
            extra_args: &[],
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: ExecIdentity {
            channel: Some("identity"),
            ..ExecIdentity::default()
        },
    };
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
    let adapter = crate::agents::find_adapter("codex").expect("codex");
    let invocation = ExecInvocation {
        kind: "codex",
        action: ExecAction::Launch {
            prompt: None,
            extra_args: &[],
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: ExecIdentity::default(),
    };

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
fn exec_argv_renders_maximal_launch_identity() {
    let extra_args = argv(&["--dangerously-skip-permissions"]);
    let invocation = ExecInvocation {
        kind: "claude",
        action: ExecAction::Launch {
            prompt: Some("fix it"),
            extra_args: &extra_args,
        },
        run_id: Some("run_123"),
        worktree_path: Some(Path::new("/repo/worktree")),
        close_pane_on_exit: true,
        exit_on_run_completion: true,
        identity: ExecIdentity {
            name: Some("swift-otter"),
            name_explicit: true,
            launch_id: Some("launch_123"),
            profile: Some("planner"),
            mode: Some(crate::harness::run::PermissionMode::Yolo),
            role: Some("coder"),
            team: Some("forge"),
            launch_group: Some("launch_group_1"),
            launch_ordinal: Some(2),
            channel: Some("design"),
            model: Some("opus"),
            effort: Some("high"),
            budget: Some("$12.50/day"),
        },
    };

    assert_eq!(
        exec_argv(Path::new("/bin/rimz"), &invocation),
        argv(&[
            "/bin/rimz",
            "agents",
            "exec",
            "claude",
            "--run-id",
            "run_123",
            "--agent-name",
            "swift-otter",
            "--agent-name-explicit",
            "--launch-id",
            "launch_123",
            "--agent-profile",
            "planner",
            "--agent-mode",
            "yolo",
            "--agent-role",
            "coder",
            "--agent-team",
            "forge",
            "--launch-group",
            "launch_group_1",
            "--launch-ordinal",
            "2",
            "--agent-channel",
            "design",
            "--agent-model",
            "opus",
            "--agent-effort",
            "high",
            "--agent-budget",
            "$12.50/day",
            "--exit-on-run-completion",
            "--close-pane-on-exit",
            "--worktree-path",
            "/repo/worktree",
            "--prompt",
            "fix it",
            "--",
            "--dangerously-skip-permissions",
        ])
    );
}

#[test]
fn exec_argv_renders_resume() {
    let extra_args = argv(&["--dangerously-skip-permissions"]);
    let invocation = ExecInvocation {
        kind: "claude",
        action: ExecAction::Resume {
            session_id: "session-1",
            extra_args: &extra_args,
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        identity: ExecIdentity {
            name: Some("swift-otter"),
            profile: Some("planner"),
            role: Some("coder"),
            team: Some("forge"),
            launch_group: Some("launch_group_1"),
            launch_ordinal: Some(2),
            channel: Some("design"),
            ..ExecIdentity::default()
        },
    };

    assert_eq!(
        exec_argv(Path::new("/bin/rimz"), &invocation),
        argv(&[
            "/bin/rimz",
            "agents",
            "exec",
            "claude",
            "--resume",
            "session-1",
            "--agent-name",
            "swift-otter",
            "--agent-profile",
            "planner",
            "--agent-role",
            "coder",
            "--agent-team",
            "forge",
            "--launch-group",
            "launch_group_1",
            "--launch-ordinal",
            "2",
            "--agent-channel",
            "design",
            "--close-pane-on-exit",
            "--",
            "--dangerously-skip-permissions",
        ])
    );
}

#[test]
fn exec_argv_renders_fork() {
    let extra_args = argv(&["--dangerously-bypass-approvals-and-sandbox"]);
    let invocation = ExecInvocation {
        kind: "codex",
        action: ExecAction::Fork {
            session_id: "session-1",
            extra_args: &extra_args,
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        identity: ExecIdentity {
            name: Some("swift-otter"),
            profile: Some("planner"),
            mode: Some(crate::harness::run::PermissionMode::Yolo),
            channel: Some("design"),
            ..ExecIdentity::default()
        },
    };

    assert_eq!(
        exec_argv(Path::new("/bin/rimz"), &invocation),
        argv(&[
            "/bin/rimz",
            "agents",
            "exec",
            "codex",
            "--fork",
            "session-1",
            "--agent-name",
            "swift-otter",
            "--agent-profile",
            "planner",
            "--agent-mode",
            "yolo",
            "--agent-channel",
            "design",
            "--close-pane-on-exit",
            "--",
            "--dangerously-bypass-approvals-and-sandbox",
        ])
    );
}

#[test]
fn exec_identity_env_maps_identity_fields() {
    let invocation = ExecInvocation {
        kind: "claude",
        action: ExecAction::Launch {
            prompt: None,
            extra_args: &[],
        },
        run_id: Some("run_123"),
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: ExecIdentity {
            name: Some("swift-otter"),
            profile: Some("planner"),
            role: Some("coder"),
            team: Some("forge"),
            launch_group: Some("launch_group_1"),
            launch_ordinal: Some(2),
            channel: Some("design"),
            model: Some("opus"),
            effort: Some("high"),
            budget: Some("$12.50/day"),
            ..ExecIdentity::default()
        },
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
                "run_123".to_owned()
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
fn launch_id_without_a_name_is_not_emitted() {
    let invocation = ExecInvocation {
        kind: "claude",
        action: ExecAction::Launch {
            prompt: None,
            extra_args: &[],
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: ExecIdentity {
            launch_id: Some("launch_orphan"),
            ..ExecIdentity::default()
        },
    };

    assert_eq!(
        exec_argv(Path::new("/bin/rimz"), &invocation),
        argv(&["/bin/rimz", "agents", "exec", "claude"])
    );
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
