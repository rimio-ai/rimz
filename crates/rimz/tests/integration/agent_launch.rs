//! Integration coverage for the supervised agent launch shell wrapper.

#[cfg(unix)]
use assert_cmd::assert::OutputAssertExt;
#[cfg(unix)]
use predicates::str::contains;
#[cfg(unix)]
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
#[cfg(unix)]
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
#[cfg(unix)]
use rimz::ids::{AgentKind, AgentSessionId};
#[cfg(unix)]
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope, EventKind};

#[cfg(unix)]
use crate::common::{
    CommandTimeoutExt, Env, canonical, exec_args, path_with_front, write_env_dump_shim,
    write_failing_agent_shim, write_fake_bash_shell, write_fake_login_shell,
};

#[cfg(unix)]
fn init_launch_repo(path: &std::path::Path) -> bool {
    std::fs::create_dir_all(path).expect("mkdir repo");
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
    };
    match run(&["init", "-q", "-b", "main"]) {
        Ok(status) if status.success() => {}
        _ => return false,
    }
    assert!(
        run(&["config", "user.email", "rimz@example.test"])
            .expect("git config email")
            .success()
    );
    assert!(
        run(&["config", "user.name", "RimZ Test"])
            .expect("git config name")
            .success()
    );
    std::fs::write(path.join("README.md"), "base\n").expect("base file");
    assert!(run(&["add", "README.md"]).expect("git add").success());
    assert!(
        run(&["commit", "-q", "-m", "base"])
            .expect("git commit")
            .success()
    );
    true
}

#[cfg(unix)]
fn fresh_exec(kind: &str, prompt: Option<&str>) -> ExecRequest {
    ExecRequest {
        kind: AgentKind::new_unchecked(kind),
        action: ExecAction::Launch {
            prompt: prompt.map(ToOwned::to_owned),
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        identity: ExecIdentity::default(),
    }
}

#[cfg(unix)]
#[test]
fn over_limit_agent_launch_refuses_before_creating_runtime_state() {
    let env = Env::new();
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace resolves");
    let launch_id = AgentSessionId::from("launch_caller");
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("codex"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("provider-caller"),
                launch_id: Some(launch_id.clone()),
                agent_name: "caller".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    launch_depth: Some(3),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched caller");

    let output = env
        .rimz()
        .args(["agents", "claude", "--worktree=depth-refused"])
        .env(rimz::harness::run::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::run::ENV_AGENT_ID, launch_id.as_str())
        .output()
        .expect("run nested launch");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "nested launch unexpectedly succeeded"
    );
    assert!(stderr.contains("launch refused"), "{stderr}");
    assert!(stderr.contains("maximum chain length of 3"), "{stderr}");
    assert!(stderr.contains("do not retry"), "{stderr}");
    assert!(!stderr.contains("--top-level"), "{stderr}");
    assert_eq!(
        env.store().read_events().expect("read events").len(),
        1,
        "refusal must not append a provisional launch"
    );
    assert!(
        !env.home_root
            .join("project-worktrees")
            .join("depth-refused")
            .exists(),
        "refusal must precede worktree creation"
    );
}

#[cfg(unix)]
#[test]
fn subagent_caller_refuses_subagent_launch_before_creating_runtime_state() {
    let env = Env::new();
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace resolves");
    let launch_id = AgentSessionId::from("launch_subagent");
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("codex"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("provider-subagent"),
                launch_id: Some(launch_id.clone()),
                agent_name: "subagent".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    parent_agent_id: Some(AgentSessionId::from("root-session")),
                    parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed subagent caller");

    let output = env
        .rimz()
        .args([
            "subagents",
            "claude",
            "try to delegate again",
            "--name",
            "subagent-refused",
        ])
        .env(rimz::harness::run::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::run::ENV_AGENT_ID, launch_id.as_str())
        .output()
        .expect("run launch from subagent");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "subagent launch unexpectedly succeeded"
    );
    assert!(stderr.contains("subagents cannot launch"), "{stderr}");
    assert!(stderr.contains("do not retry"), "{stderr}");
    assert_eq!(
        env.store().read_events().expect("read events").len(),
        1,
        "refusal must not append a provisional launch"
    );
}

#[cfg(unix)]
#[test]
fn launch_identity_and_parentage_survive_event_log_rotation() {
    let env = Env::new();
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace resolves");
    let kind = AgentKind::new_unchecked("codex");
    let agent_id = AgentSessionId::from("provider-rotated-child");
    let launch_id = AgentSessionId::from("launch-rotated-child");
    let parent_id = AgentSessionId::from("provider-parent");
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            &kind,
            AgentLaunchPayload {
                agent_id: agent_id.clone(),
                launch_id: Some(launch_id.clone()),
                agent_name: "rotated-child".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    parent_agent_id: Some(parent_id.clone()),
                    parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched child");

    let outcome = env
        .store()
        .rotate_event_log(1, None)
        .expect("rotate event log");
    assert!(outcome.rotation.is_rotated());
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            kind.as_str(),
            "UserPromptSubmit",
            &AgentLifecycleObservation::new(Some(agent_id.clone()), LifecycleSignal::TurnStarted),
        ))
        .expect("append first post-rotation lifecycle event");

    let output = env
        .rimz()
        .args(["subagents", "list", "--json"])
        .env(rimz::harness::run::ENV_AGENT_KIND, kind.as_str())
        .env(rimz::harness::run::ENV_AGENT_ID, launch_id.as_str())
        .output()
        .expect("resolve rotated launch identity through subagents list");
    assert!(
        output.status.success(),
        "rotated launch identity did not reach the caller resolver: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let projection = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("read audit projection");
    let child = projection
        .agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .expect("rotated child remains in the audit projection");
    assert_eq!(child.launch_id.as_ref(), Some(&launch_id));
    assert_eq!(child.parent_agent_id.as_ref(), Some(&parent_id));
    assert_eq!(
        child.parent_agent_kind.as_ref(),
        Some(&AgentKind::new_unchecked("claude"))
    );
    assert_eq!(child.launch_depth, Some(1));
}

#[cfg(unix)]
#[test]
fn resume_exec_attaches_only_the_resumed_session_to_its_pane() {
    let env = Env::new();
    let shim_dir = write_env_dump_shim(&env, "codex");
    let kind = AgentKind::new_unchecked("codex");
    let session_id = AgentSessionId::from("sess-resumed");
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace resolves");
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            kind.as_str(),
            "SessionStart",
            &AgentLifecycleObservation::new(Some(session_id.clone()), LifecycleSignal::Registered),
        ))
        .expect("seed resumed session");

    let dump = env.home_root.join("codex-resume.env");
    let resume = ExecRequest {
        kind: kind.clone(),
        action: ExecAction::Resume {
            session_id: session_id.to_string(),
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        identity: ExecIdentity::default(),
    };
    env.rimz()
        .args(exec_args(&resume))
        .arg("--root")
        .arg(&env.project_root)
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .env("TMUX_PANE", "%4")
        .assert_success_within_timeout("codex resume attach");

    let store = env.store();
    let attaches = store
        .read_events()
        .expect("read events")
        .into_iter()
        .filter_map(|event| match event.kind() {
            EventKind::AgentAttach(payload) => Some(payload),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(attaches.len(), 1);
    let attach = &attaches[0];
    assert_eq!(attach.agent_id, session_id);
    assert_eq!(attach.pane_id.as_str(), "tmux:%4");
    assert_eq!(attach.pane_pid, Some(attach.runtime_owner.pid));
    assert_ne!(attach.runtime_owner.pid, 0);
    assert_eq!(
        attach.runtime_owner.kind,
        rimz::pane::RuntimeOwnerKind::Agent
    );
    assert_eq!(attach.runtime_owner.subject_id, "sess-resumed");

    for action in [
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        ExecAction::Fork {
            session_id: "sess-source".to_owned(),
            extra_args: Vec::new(),
        },
    ] {
        let env = Env::new();
        let shim_dir = write_env_dump_shim(&env, "codex");
        let dump = env.home_root.join("codex-no-attach.env");
        let request = ExecRequest {
            kind: kind.clone(),
            action,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            subagent: false,
            identity: ExecIdentity::default(),
        };
        env.rimz()
            .args(exec_args(&request))
            .arg("--root")
            .arg(&env.project_root)
            .env("SHELL", "/definitely/not/a/shell")
            .env("PATH", path_with_front(&shim_dir))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
            .env("TMUX_PANE", "%4")
            .assert_success_within_timeout("codex non-resume exec");
        assert!(
            env.store()
                .read_events()
                .expect("read non-resume events")
                .iter()
                .all(|event| !matches!(event.kind(), EventKind::AgentAttach(_)))
        );
    }
}

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
        .args(exec_args(&fresh_exec("codex", None)))
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
        .args(exec_args(&fresh_exec("codex", None)))
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
fn adapter_preserves_agent_view_shell_env() {
    let env = Env::new();
    let shell = write_fake_login_shell(
        &env,
        "rimz-test-sh",
        &[("CLAUDE_CODE_DISABLE_AGENT_VIEW", "0")],
    );
    let shim_dir = write_env_dump_shim(&env, "claude");
    let dump = env.home_root.join("claude-shell.env");

    env.rimz()
        .args(exec_args(&fresh_exec("claude", None)))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("claude agent-view env launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "CLAUDE_CODE_DISABLE_AGENT_VIEW=0"),
        "claude launch env did not preserve the shell value:\n{dumped}"
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
        .args(exec_args(&fresh_exec("codex", None)))
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
        .args(exec_args(&fresh_exec("codex", None)))
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
fn supervised_cross_repo_worktree_refuses_non_terminal_input() {
    let env = Env::new();
    let current_root = env.home_root.join("current");
    if !init_launch_repo(&env.project_root) || !init_launch_repo(&current_root) {
        tracing::warn!("skipping: git unavailable");
        return;
    }
    let room_root = canonical(&env.project_root);
    let current_root = canonical(&current_root);
    let workspace_id = rimz::WorkspaceId::from_project_root(&room_root);

    env.rimz()
        .current_dir(&current_root)
        .env(rimz::workspace::ENV_WORKSPACE_ID, workspace_id.as_str())
        .env(rimz::workspace::ENV_PROJECT_ROOT, &room_root)
        .stdin(std::process::Stdio::null())
        .args([
            "agents",
            "codex",
            "-p",
            "fix the parser",
            "--worktree=supervised-cross-root",
        ])
        .assert()
        .failure()
        .stderr(contains(format!("room root: {}", room_root.display())))
        .stderr(contains(format!(
            "current git root: {}",
            current_root.display()
        )))
        .stderr(contains(format!("--root {}", current_root.display())));

    assert!(
        !env.home_root
            .join("current-worktrees")
            .join("supervised-cross-root")
            .exists(),
        "a non-terminal mismatch must refuse before creating the worktree",
    );
}

#[cfg(unix)]
#[test]
fn ambiguous_prompt_leader_refuses_before_side_effects() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "claude,claude", "do the thing"])
        .assert()
        .failure()
        .stderr(contains("this layout has several `claude` cells"))
        .stderr(contains(
            "give the first cell an inline role (`claude:lead,claude`)",
        ));

    assert!(
        !env.state_path_for(&env.project_root).events_log.exists(),
        "an ambiguous leader must not append launch events",
    );
}

#[cfg(unix)]
#[test]
fn prompt_without_an_agent_cell_refuses_before_side_effects() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "term", "do the thing"])
        .assert()
        .failure()
        .stderr(contains(
            "this layout has no agent cell to receive a prompt",
        ));

    assert!(
        !env.state_path_for(&env.project_root).events_log.exists(),
        "a missing prompt target must not append launch events",
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
        .args(exec_args(&fresh_exec("codex", Some(prompt))))
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

    let mut request = fresh_exec("codex", None);
    request.close_pane_on_exit = true;
    request.identity = ExecIdentity {
        name: Some("pruner".to_owned()),
        launch_id: Some(launch_id.to_owned()),
        params: LaunchParams {
            team: Some("trim".to_owned()),
            role: Some("pruner".to_owned()),
            ..LaunchParams::default()
        },
        ..ExecIdentity::default()
    };
    let output = env
        .rimz()
        .args(exec_args(&request))
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
            launch_id: None,
            agent_name: agent_name.to_owned(),
            agent_name_explicit: false,
            launch: LaunchParams {
                profile: None,
                mode: None,
                role: Some("pruner".to_owned()),
                model: None,
                effort: None,
                budget: None,
                team: Some("trim".to_owned()),
                launch_group: None,
                launch_ordinal: None,
                channel: None,
                kind_ordinal: Some(1),
                ..LaunchParams::default()
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
