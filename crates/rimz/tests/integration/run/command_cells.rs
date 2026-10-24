use super::*;

fn wrapper(env: &Env, run: &RunId) -> Command {
    let mut command = env.rimz();
    command
        .args([
            "--zellij",
            "agents",
            "run-command",
            "--workspace",
            env.workspace_id.as_str(),
            "--run",
            run.as_str(),
            "--",
        ])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("wrapper.log"))
        .env("ZELLIJ_PANE_ID", "7");
    command
}

fn record(env: &Env) -> RunRecord {
    env.record(&env.project_root);
    RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".into(),
        env.project_root.clone(),
    )
}

fn closed_panes(env: &Env) -> Vec<String> {
    let trace = std::fs::read_to_string(env.home_root.join("wrapper.log")).unwrap_or_default();
    trace
        .lines()
        .filter(|line| line.contains("close-pane"))
        .map(|line| line.split_whitespace().last().unwrap().to_owned())
        .collect()
}

fn assert_self_close(env: &Env) {
    assert_eq!(
        closed_panes(env),
        ["terminal_7"],
        "the wrapper must close only its own pane exactly once"
    );
}

#[test]
fn terminal_or_missing_run_starts_nothing_and_closes_only_its_own_pane() {
    let mut outcomes = Vec::new();
    for exists in [false, true] {
        let env = Env::new();
        let mut record = record(&env);
        record.status = RunStatus::Completed;
        if exists {
            rimz::harness::run::create(env.store().paths(), &record).unwrap();
        }
        let started = env.home_root.join("started");
        wrapper(&env, &record.run_id)
            .args(["touch"])
            .arg(&started)
            .assert_success_within_timeout("terminal command wrapper");
        outcomes.push((!started.exists(), closed_panes(&env)));
    }
    assert_eq!(
        outcomes,
        vec![(true, vec!["terminal_7".to_owned()]); 2],
        "terminal and absent runs must start nothing and close only their own pane"
    );
}

#[test]
fn command_exit_closes_its_pane_without_waiting_for_the_run() {
    let env = Env::new();
    let record = record(&env);
    rimz::harness::run::create(env.store().paths(), &record).unwrap();
    let started = env.home_root.join("started");
    wrapper(&env, &record.run_id)
        .arg("touch")
        .arg(&started)
        .assert_success_within_timeout("exiting command wrapper");
    assert!(started.exists(), "a live run must execute the raw command");
    assert_self_close(&env);
    assert_eq!(
        rimz::harness::run::load(env.store().paths(), &record.run_id)
            .unwrap()
            .status,
        RunStatus::Pending
    );
}

#[test]
fn empty_argv_runs_the_users_shell_and_spawn_failure_still_closes() {
    let env = Env::new();
    let record = record(&env);
    rimz::harness::run::create(env.store().paths(), &record).unwrap();
    let mut command = wrapper(&env, &record.run_id);
    command.stdin(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let _ = child
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf shell > shell-result\nexit\n");
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(env.project_root.join("shell-result")).unwrap_or_default(),
        "shell",
        "an empty argv must run the user's shell"
    );
    assert_self_close(&env);
    std::fs::remove_file(env.home_root.join("wrapper.log")).unwrap();
    let output = wrapper(&env, &record.run_id)
        .arg("/nonexistent/rimz-command-cell")
        .bounded_output()
        .unwrap();
    assert!(!output.status.success(), "spawn errors must be returned");
    assert_self_close(&env);
}

#[test]
fn terminal_run_terminates_the_command_group_then_kills_after_grace() {
    let env = Env::new();
    let record = record(&env);
    let store = env.store();
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let ready = env.home_root.join("ready");
    let terminated = env.home_root.join("terminated");
    let mut command = wrapper(&env, &record.run_id);
    command.args(["sh", "-c",
        "trap 'printf term > \"$2\"; exit 0' TERM; sh -c 'trap \"\" TERM; printf ready > \"$1\"; while :; do sleep 1; done' child \"$1\" & wait; while :; do sleep 1; done", "command"])
        .arg(&ready).arg(&terminated);
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < deadline {
        assert!(
            child.try_wait().unwrap().is_none(),
            "a live run must keep its command running"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(ready.exists(), "the command's descendant must start");
    let group = rimz::proc::children(child.id())
        .into_iter()
        .next()
        .expect("command process");
    let descendants = rimz::proc::children(group);
    rimz::harness::run::cancel(store.paths(), &record.run_id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !terminated.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        terminated.exists(),
        "TERM must reach the command within two seconds"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "terminal run must stop the command promptly"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    for pid in std::iter::once(group).chain(descendants) {
        assert!(
            !rimz::proc::process_is_live(pid, None),
            "command group member {pid} survived"
        );
    }
    assert_self_close(&env);
}
