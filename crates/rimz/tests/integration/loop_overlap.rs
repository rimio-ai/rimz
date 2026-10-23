use super::*;

fn fixture(policy: &str, limit: Option<usize>) -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    let _store = env.store();
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Tmux,
        "rimz-test",
        "sidebar",
    );
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\ncheck='true'\nsignal='trunk.moved'\noverlap={policy:?}\n{}",
            env.project_root,
            limit
                .map(|limit| format!("overlap-limit={limit}\n"))
                .unwrap_or_default()
        ),
    );
    env
}

fn holder(env: &Env) -> std::fs::File {
    hold_loop_run_lock(
        &loop_run_lock_path(env, "busy"),
        &RunLockInfo {
            pid: 42_424,
            started_at: Timestamp::now(),
        },
    )
}

fn arrive(env: &Env, from: &str, to: &str) {
    let signal = json!({"name":"trunk.moved", "source":"git", "payload":{"repo":"r", "trunk":"main", "from":from, "to":to}}).to_string();
    loop_ok(env, &["loop", "run", "busy", "--signal-json", &signal]);
}

fn arrive_at(env: &Env, from: &str, to: &str, arrived_at: Timestamp) {
    let signal = json!({"name":"trunk.moved", "source":"git", "payload":{"repo":"r", "trunk":"main", "from":from, "to":to}, "arrived_at":arrived_at}).to_string();
    loop_ok(env, &["loop", "run", "busy", "--signal-json", &signal]);
}

fn terminating_check(env: &Env) {
    std::fs::write(
        env.project_root.join("check-member.sh"),
        r#"sleep 120 &
sleeper=$!
trap 'touch term-received; while ! test -e allow-exit; do sleep 0.05; done; kill "$sleeper" 2>/dev/null; wait "$sleeper" 2>/dev/null; touch check-ended; exit 0' TERM
echo $$ > check-pid
touch check-ready
wait "$sleeper"
"#,
    )
    .unwrap();
    let check = r#"if mkdir first-check 2>/dev/null; then sh check-member.sh & member=$!; trap 'wait "$member"; exit 0' TERM; wait "$member"; else if kill -0 "$(cat check-pid)" 2>/dev/null; then touch orphan; exit 1; fi; touch next-check; fi"#;
    let path = env.rimz_home().join("loop.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        config.replace("check='true'", &format!("check={check:?}")),
    )
    .unwrap();
}

fn start_check(env: &Env) -> std::process::Child {
    let mut runner = env
        .rimz()
        .args(["loop", "run", "busy"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(env, "busy"));
    wait_for_path(&env.project_root.join("check-ready"));
    runner
}

#[test]
fn a_check_that_finishes_inside_stop_grace_keeps_its_result() {
    for cause in ["replace", "stop"] {
        let env = fixture("replace", None);
        write_loop_config(
            &env,
            &format!(
                "[tasks.busy]\nroot={:?}\ncheck='touch check-ready; while ! test -e allow-exit; do sleep 0.05; done'\nsignal='trunk.moved'\noverlap='replace'\n",
                env.project_root
            ),
        );
        let mut runner = start_check(&env);
        let stopping = env
            .rimz()
            .args([
                "loop",
                if cause == "replace" { "run" } else { "stop" },
                "busy",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while show(&env, "busy")["backlog"]["stopping"]["pid"] != runner.id() {
            assert!(Instant::now() < deadline, "stop mark missing");
            std::thread::sleep(Duration::from_millis(25));
        }
        std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
        let output = stopping.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(runner.wait().unwrap().success());
        let expected = if cause == "replace" { 3 } else { 1 };
        wait_for_loop_run_records(&env, "natural finish and drain", |rows| {
            rows.len() == expected
        });
        let rows = read_loop_run_records(&env);
        assert!(
            rows.iter().all(|row| matches!(
                row.result,
                LoopRunResult::Completed | LoopRunResult::Deferred
            )),
            "a stop request must not retype a check that finished on its own: {rows:?}"
        );
        assert!(show(&env, "busy")["backlog"]["stopping"].is_null());
    }
}

#[test]
fn an_agent_honors_its_stop_mark_before_absorbing_or_launching() {
    let env = fixture("replace", None);
    env.install_agent_hooks("claude");
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\nagent='claude'\nprompt='work'\ncheck='if mkdir first-check 2>/dev/null; then touch check-ready; while ! test -e allow-exit; do sleep 0.05; done; else exit 1; fi'\non='success'\nsignal='trunk.moved'\noverlap='replace'\n",
            env.project_root
        ),
    );
    let mut runner = start_check(&env);
    let signal =
        json!({"name":"trunk.moved", "source":"git", "payload":{"from":"a", "to":"b"}}).to_string();
    let replacement = env
        .rimz()
        .args(["loop", "run", "busy", "--signal-json", &signal])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_loop_run_records(&env, "replacement is waiting", |rows| rows.len() == 1);
    std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
    let output = replacement.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        runner.wait().unwrap().code(),
        Some(130),
        "honoring a replace must also set its exit code"
    );
    wait_for_loop_run_records(&env, "newest trigger runs its own check", |rows| {
        rows.len() == 3
    });
    let rows = read_loop_run_records(&env);
    assert_eq!(
        rows.iter().map(|row| row.result).collect::<Vec<_>>(),
        [
            LoopRunResult::Deferred,
            LoopRunResult::Replaced,
            LoopRunResult::CheckSkipped
        ],
        "{rows:?}"
    );
    assert_eq!(rows[2].signal.as_ref().unwrap().payload["to"], "b");
    assert_eq!(
        rows[2].absorbed, 0,
        "the old run must not consume the newer trigger"
    );
    assert!(
        rimz::harness::run::list(env.store().paths())
            .unwrap()
            .is_empty(),
        "neither guard permitted an agent launch"
    );
}

#[cfg(unix)]
#[test]
fn a_replace_sigterm_after_verify_keeps_the_completed_result() {
    let env = fixture("replace", None);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let panes = env.project_root.join("panes.json");
    std::fs::write(&panes, "[]").unwrap();
    let fixture = ConsumedSpawn {
        shell: write_fake_login_shell(&env, "rimz-test-sh", &[]),
        agent_bin: crate::common::write_failing_agent_shim(&env, "codex", 1),
        pane_fixture: panes,
        session_name: workspace.session_name.to_string(),
    };
    let mux = crate::common::write_path_shim(
        &env.home_root.join("mux-bin"),
        "zellij",
        r#"for arg do
    if [ "$arg" = close-pane ]; then
        touch "$RIMZ_TEST_CLOSE_READY"
        while ! test -e "$RIMZ_TEST_ALLOW_CLOSE"; do sleep 0.025; done
    fi
done
exec "$RIMZ_TEST_ZELLIJ_TRACE" "$@"
"#,
    );
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\nagent='codex'\nprompt='work'\nverify='true'\nsignal='trunk.moved'\noverlap='replace'\nthrottle='off'\n",
            env.project_root
        ),
    );
    let close_ready = env.project_root.join("close-ready");
    let allow_close = env.project_root.join("allow-close");
    let mut runner = fixture
        .command(&env)
        .args(["loop", "run", "busy"])
        .env("RIMZ_ZELLIJ_BIN", mux)
        .env(
            "RIMZ_TEST_ZELLIJ_TRACE",
            crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
        )
        .env("RIMZ_TEST_CLOSE_READY", &close_ready)
        .env("RIMZ_TEST_ALLOW_CLOSE", &allow_close)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let info = wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "busy"));
    let store = env.store();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut run = loop {
        if let Some(run) = rimz::harness::run::list(store.paths()).unwrap().first() {
            break run.clone();
        }
        assert!(Instant::now() < deadline, "run was not recorded");
        std::thread::sleep(Duration::from_millis(25));
    };
    run.pane_id = Some(rimz::ids::PaneId::parse("zellij:terminal_51").unwrap());
    run.status = RunStatus::Completed;
    run.completed_at = Some(Timestamp::now());
    rimz::harness::run::create(store.paths(), &run).unwrap();
    wait_for_path(&close_ready);
    let settled = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(settled.status, RunStatus::Completed);
    assert!(settled.verify.unwrap().passed);
    assert!(settled.joined_at.is_some());
    std::fs::write(
        store.paths().root.join("records/loop-backlog.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "tasks": {"busy": {"triggers": [],
                "stopping": {"pid": info.pid, "started_at": info.started_at, "cause": "replace"}
            }}
        }))
        .unwrap(),
    )
    .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(runner.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    std::fs::write(allow_close, "").unwrap();
    let output = runner.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let rows = read_loop_run_records(&env);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, LoopRunResult::Completed, "{rows:?}");
    assert_eq!(rows[0].run_id.as_deref(), Some(run.run_id.as_str()));
    assert!(show(&env, "busy")["backlog"]["stopping"].is_null());
    assert!(
        !loop_strikes_path(&env).exists(),
        "a completed run must not add a strike"
    );
}

#[cfg(unix)]
#[test]
fn a_late_replace_sigterm_cancels_the_run_before_draining() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let pane_fixture = env.project_root.join("panes.json");
    std::fs::write(&pane_fixture, "[]").unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let fixture = ConsumedSpawn {
        shell: write_fake_login_shell(&env, "rimz-test-sh", &[]),
        agent_bin: crate::common::write_failing_agent_shim(&env, "codex", 1),
        pane_fixture,
        session_name: workspace.session_name.to_string(),
    };
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\nagent='codex'\nprompt='work'\ncheck='true'\non='success'\nverify='touch verified; false'\nmax-attempts=2\nsignal='trunk.moved'\noverlap='replace'\nthrottle='off'\n",
            env.project_root
        ),
    );
    let mut runner = fixture
        .command(&env)
        .args(["loop", "run", "busy"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let info = wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "busy"));
    let store = env.store();
    let deadline = Instant::now() + Duration::from_secs(20);
    let run = loop {
        let records = rimz::harness::run::list(store.paths()).unwrap();
        if let Some(record) = records.first() {
            if record.status == RunStatus::Running {
                break record.clone();
            }
            if record.status == RunStatus::Pending {
                let mut hook = env.hook_command("codex");
                hook.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str())
                    .env(
                        rimz::harness::launch::ENV_AGENT_NAME,
                        record.agent_name.as_ref().unwrap().as_str(),
                    );
                let output = env
                    .spawn_payload(
                        hook,
                        &json!({
                            "hook_event_name": "SessionStart",
                            "session_id": "late-replace-session",
                            "cwd": env.project_root,
                        })
                        .to_string(),
                    )
                    .wait_with_output()
                    .unwrap();
                env.drain_hooks();
                assert!(output.status.success(), "{output:?}");
            }
        }
        if runner.try_wait().unwrap().is_some() {
            panic!("runner exited: {:?}", runner.wait_with_output().unwrap());
        }
        assert!(Instant::now() < deadline, "run never reached Running");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(run.loop_task.as_deref(), Some("busy"));
    let path = store.paths().root.join("records/loop-backlog.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::to_vec(&json!({
            "version": 1,
            "tasks": {"busy": {
                "triggers": [{"arrived_at": Timestamp::now(), "absorbed": 0,
                    "signal": {"name": "trunk.moved", "payload": {"from": "a", "to": "b"}}}],
                "stopping": {"pid": info.pid, "started_at": info.started_at, "cause": "replace"}
            }}
        }))
        .unwrap(),
    )
    .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(runner.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while runner.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "runner did not cancel");
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = runner.wait_with_output().unwrap();
    let canceled = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        canceled.status,
        RunStatus::Canceled,
        "SIGTERM must durably cancel the run, not orphan it: {output:?}"
    );
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    let next = loop {
        let records = rimz::harness::run::list(store.paths()).unwrap();
        assert!(
            records
                .iter()
                .filter(|run| !run.status.is_terminal())
                .count()
                <= 1,
            "replacement runs must never overlap: {records:?}"
        );
        if let Some(next) = records.iter().find(|record| record.run_id != run.run_id) {
            break next.clone();
        }
        assert!(Instant::now() < deadline, "waiting trigger never drained");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(next.started_at >= canceled.completed_at.unwrap());
    rimz::harness::run::cancel_and_wake(&store, &next.run_id).unwrap();
    wait_for_loop_run_records(&env, "replacement and drain recorded", |rows| {
        rows.len() == 2
    });
    let rows = read_loop_run_records(&env);
    assert_eq!(rows[0].result, LoopRunResult::Replaced, "{rows:?}");
    assert_eq!(rows[0].run_id.as_deref(), Some(run.run_id.as_str()));
    assert_eq!(rows[1].result, LoopRunResult::Canceled, "{rows:?}");
    assert_eq!(rows[1].run_id.as_deref(), Some(next.run_id.as_str()));
    assert_eq!(rows[1].signal.as_ref().unwrap().payload["to"], "b");
    assert!(!env.project_root.join("verified").exists());
    assert!(store.list_messages().unwrap().is_empty());
}

#[test]
fn a_ladder_written_replacement_notifies_when_it_disables_the_task() {
    let env = fixture("replace", None);
    let config_path = env.rimz_home().join("loop.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(config_path, format!("{config}\nmax-strikes=1\n")).unwrap();
    let notify_log = env.project_root.join("disabled-notify.log");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[notifications]\ncommand = '''printf '%s\\n' \"$RIMZ_NOTIFY_KIND\" >> '{}' '''\n",
            notify_log.display()
        ),
    )
    .unwrap();
    let lock = holder(&env);
    let mut old_runner = Command::new("sleep")
        .arg("120")
        .stdin(Stdio::from(lock.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::fs::write(
        loop_run_lock_path(&env, "busy"),
        serde_json::to_vec(&RunLockInfo {
            pid: old_runner.id(),
            started_at: Timestamp::now(),
        })
        .unwrap(),
    )
    .unwrap();
    drop(lock);
    let output = env.rimz().args(["loop", "run", "busy"]).output().unwrap();
    let _ = old_runner.kill();
    old_runner.wait().unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows = read_loop_run_records(&env);
    assert_eq!(
        rows.iter()
            .filter(|row| row.result == LoopRunResult::Replaced)
            .count(),
        1,
        "{rows:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("disabled after 1 consecutive failed fires"),
        "{output:?}"
    );
    wait_for_path(&notify_log);
    assert_eq!(
        std::fs::read_to_string(notify_log).unwrap(),
        "loop_disabled\n"
    );
}

#[test]
fn replace_uses_the_task_root_backlog_in_a_linked_worktree() {
    if crate::common::git::git_missing() {
        return;
    }
    let mut env = fixture("replace", None);
    let primary = env.project_root.clone();
    crate::common::git::init_repo(&primary);
    let checkout = env.home_root.join("linked");
    crate::common::git::git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            checkout.to_str().unwrap(),
        ],
    );
    env.project_root = canonical(&checkout);
    env.workspace_id = WorkspaceId::from_project_root(&env.project_root);
    assert_eq!(
        env.resolve_workspace(&env.project_root).project_root,
        primary
    );
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\ncheck='true'\nsignal='trunk.moved'\noverlap='replace'\n",
            env.project_root
        ),
    );
    terminating_check(&env);
    let mut runner = start_check(&env);
    std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
    arrive(&env, "a", "b");
    assert_eq!(runner.wait().unwrap().code(), Some(130));
    wait_for_loop_run_records(&env, "linked worktree replacement drains", |rows| {
        rows.len() >= 3
    });
    let rows = read_loop_run_records(&env);
    assert_eq!(
        rows.len(),
        3,
        "runner and ladder share one stop mark: {rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.result == LoopRunResult::Replaced)
            .count(),
        1,
        "{rows:?}"
    );
}

#[test]
fn stop_forwards_sigterm_to_check_group_and_records_once() {
    for agent in [false, true] {
        let env = fixture("skip", None);
        if agent {
            env.install_agent_hooks("claude");
            let path = env.rimz_home().join("loop.toml");
            let config = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, format!("{config}\nagent='claude'\nprompt='work'\n")).unwrap();
        }
        terminating_check(&env);
        let paths = env.store().paths().clone();
        if agent {
            std::fs::remove_dir_all(&paths.root).unwrap();
        }
        let mut runner = start_check(&env);
        std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
        let output = loop_ok(&env, &["loop", "stop", "busy"]);
        assert!(output.contains("SIGTERM"), "{output}");
        assert!(
            env.project_root.join("check-ended").exists(),
            "loop stop must end the check group, including an agent's check phase"
        );
        assert!(!runner.wait().unwrap().success());
        let rows = read_loop_run_records(&env);
        assert_eq!(rows.len(), 1, "runner and ladder must not both record");
        assert_eq!(rows[0].result, LoopRunResult::Canceled);
        assert_eq!(rows[0].error.as_deref(), Some("stopped by rimz loop stop"));
        if agent {
            assert!(
                !paths.workspace_record.exists(),
                "stopping an agent's cold check must not birth a room"
            );
        }
    }
}

#[test]
fn sigterm_keeps_the_lock_while_a_check_member_finishes_after_its_shell() {
    let env = fixture("skip", None);
    terminating_check(&env);
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\ncheck='sh check-member.sh & trap \"exit 0\" TERM; wait'\nevery='1h'\n",
            env.project_root
        ),
    );
    let mut runner = start_check(&env);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(runner.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    wait_for_path(&env.project_root.join("term-received"));
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        show(&env, "busy")["running"]["pid"],
        runner.id(),
        "the check group must finish before its run lock is released"
    );
    std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
    assert!(!runner.wait().unwrap().success());
    assert!(env.project_root.join("check-ended").exists());
}

#[test]
fn stop_forwards_sigterm_to_a_headless_judge() {
    let env = fixture("skip", None);
    env.write_config(&env.project_root, "[agents]\nisolation='host'\n");
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    terminating_check(&env);
    let bin = env.home_root.join("judge-bin");
    write_path_shim(
        &bin,
        "codex",
        "if [ \"$1\" = '--version' ]; then echo 'codex-cli 0.163.0'; exit 0; fi\nexec sh check-member.sh\n",
    );
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\nagent='codex'\nprompt='work'\nevery='1h'\non='any'\n[tasks.busy.check]\nagent='codex'\nprompt='Any work?'\n",
            env.project_root
        ),
    );
    let mut runner = env
        .rimz()
        .env("PATH", path_with_front(&bin))
        .env(
            "SHELL",
            crate::common::write_fake_login_shell(&env, "judge-shell", &[]),
        )
        .args(["loop", "run", "busy"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "busy"));
    wait_for_path(&env.project_root.join("check-ready"));
    std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
    loop_ok(&env, &["loop", "stop", "busy"]);
    assert!(
        env.project_root.join("check-ended").exists(),
        "the headless judge must receive SIGTERM, not be orphaned or killed outright"
    );
    assert!(!runner.wait().unwrap().success());
    let rows = read_loop_run_records(&env);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, LoopRunResult::Canceled);
}

#[test]
fn replace_coalesces_without_stopping_a_throttle_held_runner() {
    let env = fixture("replace", None);
    env.install_agent_hooks("claude");
    let path = env.rimz_home().join("loop.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        format!("{config}\nagent='claude'\nprompt='work'\non='any'\n"),
    )
    .unwrap();
    loop_ok(&env, &["config", "set", "loop.throttle.pace", "1h"]);
    let queue = env.rimz_home().join("loops/throttle");
    std::fs::create_dir_all(&queue).unwrap();
    std::fs::write(queue.join("00000000000000000001-01890000-0000-7000-8000-000000000000"), json!({"owner":{"pid":std::process::id(),"start":null},"task":"other","root":"/elsewhere","checkout":"/elsewhere","enqueued_ms":1,"state":"admitted"}).to_string()).unwrap();
    let mut runner = env
        .rimz()
        .args(["loop", "run", "busy"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "busy"));
    let deadline = Instant::now() + Duration::from_secs(30);
    while show(&env, "busy")["held"]["pid"] != runner.id() {
        assert!(
            Instant::now() < deadline,
            "runner did not reach the throttle"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    arrive(&env, "a", "b");
    arrive(&env, "b", "c");
    let value = show(&env, "busy");
    assert!(value["backlog"]["stopping"].is_null());
    assert_eq!(value["backlog"]["triggers"][0]["absorbed"], 1);
    assert!(
        runner.try_wait().unwrap().is_none(),
        "replace must not stop a throttle-held run"
    );
    loop_ok(&env, &["loop", "pause", "busy", "--for", "1h"]);
    loop_ok(&env, &["loop", "stop", "busy"]);
    runner.wait().unwrap();
    assert_eq!(
        read_loop_run_records(&env)
            .iter()
            .map(|row| row.result)
            .collect::<Vec<_>>(),
        [
            LoopRunResult::Deferred,
            LoopRunResult::Deferred,
            LoopRunResult::Canceled
        ]
    );
}

#[test]
fn replace_waits_for_check_exit_and_serves_latest_without_a_second_stop() {
    let env = fixture("replace", None);
    terminating_check(&env);
    let mut runner = start_check(&env);
    let signal = json!({"name":"trunk.moved", "source":"git", "payload":{"repo":"r", "trunk":"main", "from":"a", "to":"b"}}).to_string();
    let replacement = env
        .rimz()
        .args(["loop", "run", "busy", "--signal-json", &signal])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_loop_run_records(&env, "replace records the waiting trigger", |rows| {
        rows.len() == 1
    });
    let value = show(&env, "busy");
    assert_eq!(
        value["backlog"]["stopping"]["pid"],
        runner.id(),
        "replace must mark its holder before signaling"
    );
    assert_eq!(value["backlog"]["stopping"]["cause"], "replace");
    assert!(
        value["runs"][0]["error"]
            .as_str()
            .unwrap()
            .ends_with("stopping it for this trigger")
    );
    let text = loop_ok(&env, &["loop", "show", "busy"]);
    assert!(
        text.contains(&format!(" · stopping pid {}", runner.id())),
        "{text}"
    );
    arrive(&env, "b", "c");
    assert_eq!(
        show(&env, "busy")["backlog"]["stopping"],
        value["backlog"]["stopping"]
    );
    wait_for_path(&env.project_root.join("term-received"));
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        runner.try_wait().unwrap().is_none(),
        "SIGTERM must not escalate after the Ctrl-C grace"
    );
    assert!(!env.project_root.join("next-check").exists());
    std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
    let output = replacement.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!runner.wait().unwrap().success());
    wait_for_loop_run_records(
        &env,
        "replacement drains after the old check exits",
        |rows| rows.len() == 4,
    );
    let rows = read_loop_run_records(&env);
    assert_eq!(
        rows.iter().map(|row| row.result).collect::<Vec<_>>(),
        [
            LoopRunResult::Deferred,
            LoopRunResult::Deferred,
            LoopRunResult::Replaced,
            LoopRunResult::Completed
        ]
    );
    assert_eq!(
        rows[2].error.as_deref(),
        Some("replaced by a newer trigger")
    );
    assert_eq!(rows[3].signal.as_ref().unwrap().payload["to"], "c");
    assert_eq!(rows[3].absorbed, 1);
    assert!(env.project_root.join("check-ended").exists());
    assert!(env.project_root.join("next-check").exists());
    assert!(
        !env.project_root.join("orphan").exists(),
        "the next check must not start beside the old one"
    );
    assert!(show(&env, "busy")["backlog"]["stopping"].is_null());
}

#[test]
fn stop_keeps_waiting_trigger_and_reports_its_drain() {
    let env = fixture("coalesce", None);
    terminating_check(&env);
    let mut runner = start_check(&env);
    arrive(&env, "a", "b");
    std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
    let output = loop_ok(&env, &["loop", "stop", "busy"]);
    assert!(
        output.contains("1 trigger waiting")
            && output.contains("next starts now")
            && output.contains("rimz loop pause busy"),
        "{output}"
    );
    assert!(!runner.wait().unwrap().success());
    wait_for_loop_run_records(&env, "stop drains waiting work", |rows| rows.len() == 3);
    let rows = read_loop_run_records(&env);
    assert_eq!(
        rows.iter().map(|row| row.result).collect::<Vec<_>>(),
        [
            LoopRunResult::Deferred,
            LoopRunResult::Canceled,
            LoopRunResult::Completed
        ]
    );
    assert!(!env.project_root.join("orphan").exists());
}

#[test]
fn a_failed_replace_ladder_keeps_waiting_then_the_runner_records_and_drains() {
    let env = fixture("replace", None);
    terminating_check(&env);
    let mut runner = start_check(&env);
    arrive(&env, "a", "b");
    assert!(
        env.project_root.join("term-received").exists(),
        "replace must attempt to stop the check"
    );
    assert!(
        runner.try_wait().unwrap().is_none(),
        "the ladder must leave the check alive past its own grace"
    );
    assert_eq!(
        read_loop_run_records(&env).len(),
        1,
        "a failed ladder must not write the holder's row"
    );
    assert_eq!(
        show(&env, "busy")["backlog"]["stopping"]["pid"],
        runner.id()
    );
    arrive(&env, "b", "c");
    std::fs::write(env.project_root.join("allow-exit"), "").unwrap();
    assert!(!runner.wait().unwrap().success());
    wait_for_loop_run_records(
        &env,
        "runner finishes a replace after the ladder returned",
        |rows| rows.len() == 4,
    );
    let rows = read_loop_run_records(&env);
    assert_eq!(
        rows.iter()
            .filter(|row| row.result == LoopRunResult::Replaced)
            .count(),
        1
    );
    assert_eq!(rows[3].result, LoopRunResult::Completed);
    assert_eq!(rows[3].signal.as_ref().unwrap().payload["to"], "c");
}

fn show(env: &Env, name: &str) -> serde_json::Value {
    serde_json::from_str(&loop_ok(env, &["loop", "show", name, "--json"])).unwrap()
}

#[test]
fn coalesce_records_each_arrival_and_drains_the_merged_newest_trigger() {
    let env = fixture("coalesce", None);
    let lock = holder(&env);
    arrive(&env, "a", "b");
    arrive(&env, "b", "c");
    let records = read_loop_run_records(&env);
    assert_eq!(
        records
            .iter()
            .map(|row| row.result.label())
            .collect::<Vec<_>>(),
        ["deferred", "deferred"]
    );
    let text = loop_ok(&env, &["loop", "show", "busy"]);
    assert!(
        text.contains("  1 trigger waiting · runs when the active run ends · absorbed 1\n"),
        "{text}"
    );
    let backlog = &show(&env, "busy")["backlog"];
    assert_eq!(backlog["policy"], "coalesce");
    assert!(backlog["limit"].is_null());
    assert!(backlog["stopping"].is_null());
    assert_eq!(backlog["triggers"][0]["signal"]["payload"]["from"], "a");
    assert_eq!(backlog["triggers"][0]["signal"]["payload"]["to"], "c");
    assert_eq!(backlog["triggers"][0]["absorbed"], 1);
    loop_ok(&env, &["loop", "run", "busy", "--drain"]);
    assert_eq!(
        read_loop_run_records(&env).len(),
        2,
        "busy drainer writes no row"
    );
    lock.unlock().unwrap();
    loop_ok(&env, &["loop", "run", "busy", "--drain"]);
    let value = show(&env, "busy");
    assert_eq!(value["runs"].as_array().unwrap().len(), 3);
    assert_eq!(value["runs"][2]["result"], "completed");
    assert_eq!(value["runs"][2]["absorbed"], 1);
    assert_eq!(value["runs"][2]["signal"], backlog["triggers"][0]["signal"]);
    assert!(value["backlog"]["triggers"].as_array().unwrap().is_empty());
    loop_ok(&env, &["loop", "run", "busy", "--drain"]);
    assert_eq!(
        read_loop_run_records(&env).len(),
        3,
        "empty drainer writes no row"
    );
}

#[test]
fn one_signal_firing_stamps_all_waiting_subscribers_identically() {
    let env = fixture("coalesce", None);
    let config = std::fs::read_to_string(env.rimz_home().join("loop.toml"))
        .unwrap()
        .replace("trunk.moved", "build.moved");
    write_loop_config(
        &env,
        &format!(
            "{config}\n{}",
            config.replace("[tasks.busy]", "[tasks.other]")
        ),
    );
    let _busy = holder(&env);
    let _other = hold_loop_run_lock(
        &loop_run_lock_path(&env, "other"),
        &RunLockInfo {
            pid: 42_424,
            started_at: Timestamp::now(),
        },
    );
    loop_ok(&env, &["events", "emit", "build.moved"]);
    wait_for_loop_run_records(&env, "both signal subscribers deferred", |records| {
        records.len() == 2
    });
    assert_eq!(
        show(&env, "busy")["backlog"]["triggers"][0]["arrived_at"],
        show(&env, "other")["backlog"]["triggers"][0]["arrived_at"]
    );
}

#[test]
fn coalesce_drains_the_latest_fired_signal_when_runners_arrive_backwards() {
    let env = fixture("coalesce", None);
    let lock = holder(&env);
    arrive_at(&env, "b", "c", Timestamp::from_second(2).unwrap());
    arrive_at(&env, "a", "b", Timestamp::from_second(1).unwrap());
    lock.unlock().unwrap();
    loop_ok(&env, &["loop", "run", "busy", "--drain"]);
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Completed);
    assert_eq!(record.absorbed, 1);
    assert_eq!(
        record.signal.unwrap().payload,
        *json!({"repo":"r", "trunk":"main", "from":"a", "to":"c"})
            .as_object()
            .unwrap()
    );
}

#[test]
fn queue_drains_in_firing_order_when_runners_arrive_backwards() {
    let env = fixture("queue", None);
    let lock = holder(&env);
    arrive_at(&env, "b", "c", Timestamp::from_second(2).unwrap());
    arrive_at(&env, "a", "b", Timestamp::from_second(1).unwrap());
    lock.unlock().unwrap();
    loop_ok(&env, &["loop", "run", "busy", "--drain"]);
    wait_for_loop_run_records(&env, "queue drains in firing order", |records| {
        records.len() == 4
    });
    let records = read_loop_run_records(&env);
    assert_eq!(records[2].result, LoopRunResult::Completed);
    assert_eq!(records[2].signal.as_ref().unwrap().payload["to"], "b");
    assert_eq!(records[3].result, LoopRunResult::Completed);
    assert_eq!(records[3].signal.as_ref().unwrap().payload["to"], "c");
    assert!(records[2..].iter().all(|record| record.absorbed == 0));
}

#[test]
fn queue_is_fifo_full_is_overlapped_and_release_starts_the_next_process() {
    let env = fixture("queue", Some(2));
    let lock = holder(&env);
    for (from, to) in [("a", "b"), ("b", "c"), ("c", "d")] {
        arrive(&env, from, to);
    }
    let records = read_loop_run_records(&env);
    assert_eq!(
        records
            .iter()
            .map(|row| row.result.label())
            .collect::<Vec<_>>(),
        ["deferred", "deferred", "overlapped"]
    );
    assert!(
        records[2]
            .error
            .as_deref()
            .unwrap()
            .contains("queue full (2/2), skipped")
    );
    assert!(
        loop_ok(&env, &["loop", "show", "busy"])
            .contains("  queue 2/2 · next runs when the active run ends\n")
    );
    let value = show(&env, "busy");
    assert_eq!(value["backlog"]["limit"], 2);
    lock.unlock().unwrap();
    loop_ok(&env, &["loop", "run", "busy", "--drain"]);
    wait_for_loop_run_records(&env, "queue drains both triggers", |records| {
        records.len() == 5
    });
    let records = read_loop_run_records(&env);
    assert_eq!(records[3].signal.as_ref().unwrap().payload["to"], "b");
    assert_eq!(records[4].signal.as_ref().unwrap().payload["to"], "c");
    assert!(
        serde_json::to_value(&records[3])
            .unwrap()
            .get("absorbed")
            .is_none()
    );
    assert!(
        show(&env, "busy")["backlog"]["triggers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fresh_arrival_reconciles_waiting_triggers_and_manual_overlap_never_enqueues() {
    let env = fixture("coalesce", None);
    let lock = holder(&env);
    arrive(&env, "a", "b");
    assert_eq!(last_loop_record(&env).result.label(), "deferred");
    let text = loop_ok(&env, &["loop", "fire", "busy"]);
    assert!(
        text.contains("stop it with `rimz loop stop busy`"),
        "{text}"
    );
    assert_eq!(
        show(&env, "busy")["backlog"]["triggers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    lock.unlock().unwrap();
    arrive(&env, "b", "c");
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Completed);
    assert_eq!(
        record.signal.unwrap().payload,
        *json!({"repo":"r", "trunk":"main", "from":"a", "to":"c"})
            .as_object()
            .unwrap()
    );
    assert_eq!(
        serde_json::to_value(&show(&env, "busy")["runs"][2]).unwrap()["absorbed"],
        1
    );
}

#[test]
fn drain_drops_a_task_removed_by_hand_edit_without_a_row() {
    let env = fixture("coalesce", None);
    write_loop_config(&env, "");
    assert_unrunnable_drain_discards_backlog(&env);
}

#[test]
fn drain_drops_a_task_broken_by_hand_edit_without_a_row() {
    let env = fixture("coalesce", None);
    write_loop_config(
        &env,
        &format!(
            "[tasks.busy]\nroot={:?}\nagent='claude'\nprompt='work'\nevery='1h'\nstay=true\noverlap='coalesce'\n",
            env.project_root
        ),
    );
    assert_unrunnable_drain_discards_backlog(&env);
}

#[test]
fn drain_drops_definitions_that_cannot_enqueue_without_a_row() {
    for definition in [
        "check='true'\nevery='1h'",
        "check='true'\nevery='1h'\noverlap='skip'",
        "agent='claude'\nprompt='work'\nevery='1h'\nstay=true\neach-worktree=true",
        "check='true'\nwhen=['pr=merged']",
        "wait={kind='claude',session='s',handle='@claude'}\nevery='1h'\nprompt='work'",
        "workflow='missing'\nevery='1h'",
        "check='true'",
    ] {
        let env = fixture("coalesce", None);
        write_loop_config(
            &env,
            &format!("[tasks.busy]\nroot={:?}\n{definition}\n", env.project_root),
        );
        assert_unrunnable_drain_discards_backlog(&env);
    }
}

fn assert_unrunnable_drain_discards_backlog(env: &Env) {
    use rimz::harness::schedule::backlog;

    let paths = env.store().paths().clone();
    let now = Timestamp::now();
    let waiting = json!({
        "triggers": [
            {"arrived_at": now, "signal": null, "absorbed": 0},
            {"arrived_at": now, "signal": null, "absorbed": 0}
        ],
        "stopping": {"pid": 42_424, "started_at": now, "cause": "replace"}
    });
    let path = paths.root.join("records/loop-backlog.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::to_vec(&json!({
            "version": 1,
            "tasks": {"busy": waiting, "other": waiting}
        }))
        .unwrap(),
    )
    .unwrap();
    let output = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["loop", "run", "busy", "--drain"])
        .current_dir(&env.home_root)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "an unrunnable drain must discard its backlog and exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(read_loop_run_records(env).is_empty(), "drop writes no row");
    let remaining = backlog::load(&paths).unwrap();
    assert!(!remaining.tasks.contains_key("busy"));
    assert_eq!(remaining.tasks.len(), 1, "other task backlogs must survive");
    assert_eq!(
        serde_json::to_value(&remaining.tasks["other"]).unwrap(),
        waiting
    );
}

#[test]
fn paused_drain_drops_triggers_without_a_row() {
    let env = fixture("queue", Some(2));
    let lock = holder(&env);
    arrive(&env, "a", "b");
    arrive(&env, "b", "c");
    assert_eq!(last_loop_record(&env).result.label(), "deferred");
    loop_ok(&env, &["loop", "pause", "busy", "--for", "1h"]);
    lock.unlock().unwrap();
    loop_ok(&env, &["loop", "run", "busy", "--drain"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !show(&env, "busy")["backlog"]["triggers"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline, "paused backlog did not drain");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(read_loop_run_records(&env).len(), 2);
}

#[test]
fn catalog_rename_moves_backlog_and_redefinition_and_removal_clear_it() {
    for operation in ["rename", "replace", "remove"] {
        let env = fixture("coalesce", None);
        let lock = holder(&env);
        arrive(&env, "a", "b");
        assert_eq!(last_loop_record(&env).result.label(), "deferred");
        match operation {
            "rename" => {
                loop_ok(&env, &["loop", "rename", "busy", "renamed"]);
                assert_eq!(
                    show(&env, "renamed")["backlog"]["triggers"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
            }
            "replace" => {
                loop_ok(
                    &env,
                    &[
                        "loop",
                        "add",
                        "busy",
                        "--check",
                        "true",
                        "--every",
                        "1h",
                        "--overlap",
                        "coalesce",
                    ],
                );
                assert!(
                    show(&env, "busy")["backlog"]["triggers"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
            }
            _ => {
                loop_ok(&env, &["loop", "remove", "busy"]);
            }
        }
        assert!(
            !rimz::harness::schedule::backlog::load(env.store().paths())
                .unwrap()
                .tasks
                .contains_key("busy")
        );
        lock.unlock().unwrap();
    }
}

#[test]
fn overlap_flags_persist_for_machine_and_project_tasks() {
    for project in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        let mut flags = vec![
            "loop",
            "add",
            "busy",
            "--check",
            "true",
            "--every",
            "1h",
            "--overlap",
            "queue",
            "--overlap-limit",
            "3",
        ];
        if project {
            flags.push("--project");
        }
        let output = env.rimz().args(&flags).output().unwrap();
        assert!(
            output.status.success(),
            "overlap flags must be accepted: {output:?}"
        );
        let entry = &show(&env, "busy")["entry"];
        assert_eq!(entry["overlap"], "queue");
        assert_eq!(entry["overlap-limit"], 3);
    }
}

#[test]
fn add_refuses_overlap_on_unsupported_shapes_and_invalid_limits() {
    let env = Env::new();
    for (flags, reason) in [
        (
            vec!["--agent", "claude", "--stay", "--every", "1h"],
            "a resident task launches once per checkout",
        ),
        (
            vec!["--check", "true", "--when", "team.stage=Done"],
            "clock and signal triggers",
        ),
        (vec!["--check", "true", "--at", "07:00"], "one-shot"),
        (
            vec!["--wait", "--signal", "deploy.done"],
            "no run to overlap",
        ),
    ] {
        let output = env
            .rimz()
            .args([vec!["loop", "add", "busy", "--overlap", "coalesce"], flags].concat())
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && error.contains("--overlap") && error.contains(reason),
            "{error}"
        );
    }
    for (flags, reason) in [
        (vec!["--overlap-limit", "2"], "queue"),
        (
            vec!["--overlap", "queue", "--overlap-limit", "0"],
            "at least 1",
        ),
    ] {
        let output = env
            .rimz()
            .args(
                [
                    vec!["loop", "add", "busy", "--check", "true", "--every", "1h"],
                    flags,
                ]
                .concat(),
            )
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && error.contains("--overlap-limit") && error.contains(reason),
            "{error}"
        );
    }
}

#[test]
fn loaded_machine_and_project_tasks_refuse_invalid_overlap_fields() {
    for project in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        let fields =
            "[tasks.busy]\ncheck='true'\nevery='1h'\noverlap='coalesce'\noverlap-limit=2\n";
        if project {
            std::fs::create_dir_all(env.project_root.join(".rimz")).unwrap();
            std::fs::write(env.project_root.join(".rimz/config.toml"), fields).unwrap();
        } else {
            write_loop_config(&env, &format!("{fields}root={:?}\n", env.project_root));
        }
        let text = loop_ok(&env, &["loop", "show", "busy"]);
        assert!(
            text.contains("overlap-limit") && text.contains("queue"),
            "{text}"
        );
    }
}

#[test]
fn drain_rechecks_project_trust_and_records_its_refusal() {
    let env = fixture("coalesce", None);
    let lock = holder(&env);
    arrive(&env, "a", "b");
    write_loop_config(&env, "");
    std::fs::create_dir_all(env.project_root.join(".rimz")).unwrap();
    std::fs::write(
        env.project_root.join(".rimz/config.toml"),
        "[tasks.busy]\ncheck='true'\nsignal='trunk.moved'\noverlap='coalesce'\n",
    )
    .unwrap();
    loop_ok(&env, &["loop", "enable", "busy"]);
    lock.unlock().unwrap();
    let output = env
        .rimz()
        .args(["loop", "run", "busy", "--drain"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let rows = read_loop_run_records(&env);
    assert_eq!(
        rows.len(),
        2,
        "a drained trigger refused by trust still gets its gate row"
    );
    assert_eq!(rows[1].result, LoopRunResult::Errored);
    assert!(rows[1].error.as_deref().unwrap().contains("trust"));
    assert!(
        show(&env, "busy")["backlog"]["triggers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
