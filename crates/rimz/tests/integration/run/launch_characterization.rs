use super::*;

fn launch_command(env: &Env) -> Command {
    let workspace = env.resolve_workspace(&env.project_root);
    env.record(&env.project_root);
    seed_live_zellij_room(&env.runtime_paths(), &workspace.session_name, Vec::new());
    let agent_bin = write_failing_agent_shim(env, "codex", 1);
    let mut command = env.rimz();
    command
        .args(["--mux", "zellij", "agents", "codex", "fix it"])
        .env("PATH", path_with_front(&agent_bin))
        .env("SHELL", write_fake_login_shell(env, "rimz-test-sh", &[]))
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("launch.log"))
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .env("RIMZ_TEST_ZELLIJ_TOPOLOGY_PANES", "[]")
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        );
    command
}

#[test]
fn pending_record_and_bound_waiter_precede_an_instant_completion_at_open() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let output = launch_command(&env)
        .args(["-p", "--new-tab"])
        .env(
            "RIMZ_TEST_ZELLIJ_COMPLETE_RUN_AT_OPEN",
            env.workspace_id.as_str(),
        )
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "completed at pane open\n"
    );
    let trace = std::fs::read_to_string(env.home_root.join("launch.log")).unwrap();
    assert!(trace.contains("completed-at-open"), "{trace}");
}

#[test]
fn only_the_run_bearing_recipe_leaves_a_checkout_after_hook_refusal() {
    if crate::common::git::git_missing() {
        return;
    }
    let mut observed = Vec::new();
    for supervised in [false, true] {
        let env = Env::new();
        crate::common::git::init_repo(&env.project_root);
        let mut command = launch_command(&env);
        command.args([
            "--worktree=preflight-refusal",
            "--new-tab",
            "--isolation",
            "host",
        ]);
        if supervised {
            command.args(["-p", "--bg"]);
        }
        let output = command.bounded_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stderr}");
        assert!(stderr.contains("hooks"), "{stderr}");
        assert!(
            rimz::harness::run::list(env.store().paths())
                .unwrap()
                .is_empty()
        );
        let checkout = env.home_root.join("project-worktrees/preflight-refusal");
        observed.push(checkout.is_dir());
    }
    assert_eq!(observed, [false, true]);
}

#[test]
fn path_probe_is_strict_for_runs_and_skipped_for_layouts_without_a_version_floor() {
    let mut observed = Vec::new();
    for supervised in [false, true] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        crate::common::trust_claude_project(&env, &env.project_root);
        let _ = launch_command(&env);
        let shell = env.home_root.join("shell-bin/rimz-test-sh");
        let body = std::fs::read_to_string(&shell).unwrap().replacen(
            "#!/bin/sh\n",
            "#!/bin/sh\ncase \"$*\" in *__RIMZ_ENV_PROBE_START__*) printf 'probe\\n' >> \"$RIMZ_TEST_PATH_PROBES\" ;; esac\n",
            1,
        );
        std::fs::write(&shell, body).unwrap();
        let log = env.home_root.join("path-probes");
        let agent_bin = write_failing_agent_shim(&env, "claude", 1);
        let workspace = env.resolve_workspace(&env.project_root);
        let mut args = vec![
            "--mux",
            "zellij",
            "agents",
            "claude",
            "fix it",
            "--new-tab",
            "--isolation",
            "host",
        ];
        if supervised {
            args.extend(["-p", "--bg"]);
        }
        let mut command = env.rimz();
        let output = command
            .args(args)
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env("RIMZ_TEST_PATH_PROBES", &log)
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("launch.log"))
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env("RIMZ_TEST_ZELLIJ_TOPOLOGY_PANES", "[]")
            .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            )
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        observed.push(
            std::fs::read_to_string(log)
                .unwrap_or_default()
                .lines()
                .count(),
        );
    }
    assert_eq!(observed, [0, 1]);
}

#[test]
fn retry_reuses_preflight_even_when_the_login_shell_disappears() {
    let env = Env::new();
    let store = env.store();
    let mut child = spawn_retrying_print(&env, "preflight-retry", false, "fix it");
    let mut first = wait_for_run_count(&store, &mut child, 1).pop().unwrap();
    std::fs::remove_file(env.home_root.join("shell-bin/rimz-test-sh")).unwrap();
    first.status = RunStatus::Failed;
    finish_run(&store, &mut first);
    let records = wait_for_run_count(&store, &mut child, 2);
    let mut retry = records
        .into_iter()
        .find(|run| run.retry_of.as_ref() == Some(&first.run_id))
        .unwrap();
    retry.status = RunStatus::Completed;
    retry.last_message = Some("reused preparation".into());
    end_retry_agents(&env, &store);
    finish_run(&store, &mut retry);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "reused preparation\n"
    );
}
