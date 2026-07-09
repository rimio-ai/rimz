//! Integration coverage for the supervised agent launch shell wrapper.

#[cfg(unix)]
use assert_cmd::assert::OutputAssertExt;
#[cfg(unix)]
use predicates::str::contains;
#[cfg(unix)]
use rimz::agents::LaunchParams;
#[cfg(unix)]
use rimz::ids::{AgentKind, AgentSessionId};
#[cfg(unix)]
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

#[cfg(unix)]
use crate::common::{
    CommandTimeoutExt, Env, path_with_front, write_env_dump_shim, write_failing_agent_shim,
    write_fake_bash_shell, write_fake_login_shell,
};

#[cfg(unix)]
#[test]
fn shell_rc_env_reaches_the_spawned_agent() {
    let env = Env::new();
    let shell = write_fake_login_shell(
        &env,
        "rimz-test-sh",
        &[("RIMZ_TEST_RC_MARKER", "from-shell")],
    );
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-shell.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex shell rc launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_RC_MARKER=from-shell"),
        "agent process env misses the shell rc marker:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn bashrc_path_reaches_the_spawned_agent() {
    let env = Env::new();
    let shell = write_fake_bash_shell(&env);
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        env.home_root.join(".bashrc"),
        format!(
            "export PATH='{}':\"$PATH\"\nexport RIMZ_TEST_BASHRC_MARKER=from-bashrc\n",
            shim_dir.display()
        ),
    )
    .expect("write bashrc");
    let dump = env.home_root.join("codex-bashrc.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", &shell)
        .env("PATH", "/usr/bin:/bin")
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex bashrc launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_BASHRC_MARKER=from-bashrc"),
        "agent process env misses the bashrc marker:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn adapter_pin_overrides_shell_rc_env() {
    let env = Env::new();
    let shell = write_fake_login_shell(
        &env,
        "rimz-test-sh",
        &[("CLAUDE_CODE_DISABLE_AGENT_VIEW", "0")],
    );
    let shim_dir = write_env_dump_shim(&env, "claude");
    let dump = env.home_root.join("claude-shell.env");

    env.rimz()
        .args(["agents", "exec", "claude"])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("claude adapter pin launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "CLAUDE_CODE_DISABLE_AGENT_VIEW=1"),
        "claude launch env misses the built-in pin after shell rc:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn trusted_agent_env_overrides_shell_rc_env() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"codex\"\nenv = { RIMZ_TEST_CONFIGURED = \"trusted\" }\n",
    );
    env.rimz().args(["trust", "grant"]).assert().success();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[("RIMZ_TEST_CONFIGURED", "rc")]);
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-trusted-shell.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex trusted env launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_CONFIGURED=trusted"),
        "trusted launch env did not override the shell rc value:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn missing_shell_path_falls_back_to_direct_exec() {
    let env = Env::new();
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-direct.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex direct launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGV="),
        "direct fallback did not run the agent shim:\n{dumped}"
    );
}

/// An invalid explicit `--new-pane` (here a multi-cell layout) refuses the
/// whole launch before any side effect, so it leaves no provisional store rows
/// and never creates the requested worktree. Resolution runs ahead of the
/// live-session probe, so the rejection needs neither a running room nor a mux.
#[cfg(unix)]
#[test]
fn invalid_new_pane_refuses_an_agents_launch_before_side_effects() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "claude,codex", "--worktree=wt-a", "--new-pane"])
        .assert()
        .failure()
        .stderr(contains("single agent cell"));

    assert!(
        !env.home_root
            .join("project-worktrees")
            .join("wt-a")
            .exists(),
        "a rejected --new-pane must not create the worktree",
    );
    assert!(
        !env.state_path_for(&env.project_root).events_log.exists(),
        "a rejected --new-pane must not append launch events",
    );
}

#[cfg(unix)]
#[test]
fn resume_with_empty_store_refuses_before_mux_probe() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "claude", "--resume"])
        .assert()
        .failure()
        .stderr(contains(
            "nothing to resume for `claude`; launch without `--resume`",
        ));
}

#[cfg(unix)]
#[test]
fn prompt_with_shell_metacharacters_stays_one_argument_after_terminator() {
    let env = Env::new();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-prompt.env");
    let prompt = r#"say "hello there" with spaces"#;

    env.rimz()
        .args(["agents", "exec", "codex", "--prompt", prompt])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex prompt launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGC=2"),
        "launch argv did not contain only the terminator and prompt:\n{dumped}"
    );
    assert!(
        dumped.lines().any(|line| line == "ARGV_1=--"),
        "launch argv did not protect the prompt with --:\n{dumped}"
    );
    assert!(
        dumped
            .lines()
            .any(|line| line == format!("ARGV_2={prompt}")),
        "prompt argv element was changed by the shell wrapper:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn close_pane_exec_reports_startup_failure_before_dropping_to_shell() {
    let env = Env::new();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let shim_dir = write_failing_agent_shim(&env, "codex", 7);
    let idle_shell_marker = env.home_root.join("idle-shell.marker");
    let launch_id = "launch_startup_failure";
    seed_provisional_agent_launch(&env, launch_id, "pruner");

    let output = env
        .rimz()
        .args([
            "agents",
            "exec",
            "codex",
            "--launch-id",
            launch_id,
            "--agent-name",
            "pruner",
            "--agent-team",
            "trim",
            "--agent-role",
            "pruner",
            "--close-pane-on-exit",
        ])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_IDLE_SHELL_MARKER", &idle_shell_marker)
        .bounded_output()
        .expect("agents exec returns without waiting on non-tty stdin");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&idle_shell_marker).expect("idle shell marker"),
        "idle shell\n"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to start"), "{stderr}");
    assert!(stderr.contains("exit status: 7"), "{stderr}");
    assert!(stderr.contains("rimz agents trim.pruner"), "{stderr}");
}

#[cfg(unix)]
fn seed_provisional_agent_launch(env: &Env, launch_id: &str, agent_name: &str) {
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let kind = AgentKind::new_unchecked("codex");
    let event = EventEnvelope::agent_launched(
        workspace.workspace_id,
        workspace.session_name,
        &kind,
        AgentLaunchPayload {
            agent_id: AgentSessionId::from(launch_id),
            agent_name: agent_name.to_owned(),
            agent_name_explicit: false,
            launch: LaunchParams {
                profile: None,
                role: Some("pruner".to_owned()),
                model: None,
                effort: None,
                budget: None,
                team: Some("trim".to_owned()),
                launch_group: None,
                launch_ordinal: None,
                channel: None,
                kind_ordinal: Some(1),
            },
            state: AgentLaunchState::Starting,
            run_id: None,
            pane_id: None,
            runtime_owner: None,
            worktree_path: Some(env.project_root.display().to_string()),
            worktree_branch: None,
            prompt: None,
            description: None,
        },
    );
    env.store().append_event(&event).expect("append launch");
}
