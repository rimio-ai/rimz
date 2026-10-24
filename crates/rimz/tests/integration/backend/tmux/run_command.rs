use super::support::*;
use rimz::agents::PermissionMode;
use rimz::store::run::RunRecord;

#[test]
fn terminal_run_closes_only_its_command_pane() {
    check_command_pane(false);
}

#[test]
fn command_wrapper_holds_under_8_mib() {
    if !cfg!(target_os = "linux") {
        crate::common::skip("Linux smaps_rollup not available");
        return;
    }
    check_command_pane(true);
}

fn check_command_pane(measure_memory: bool) {
    require_tmux!();
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let record = RunRecord::new(
        workspace.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".into(),
        env.project_root.clone(),
    );
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    server
        .backend
        .ensure_session(&session_opts(
            &workspace.session_name,
            workspace.workspace_id.clone(),
            &env.project_root,
            &env.project_root,
            Some((100, 30)),
        ))
        .unwrap();
    server.tmux(&["set-option", "-g", "remain-on-exit", "on"]);
    let unrelated = list_session_panes(&server, &workspace.session_name)[0]
        .pane_id
        .clone();
    let ready = env.home_root.join("command-ready");
    let received = env.home_root.join("command-input");
    let command = env.rimz();
    let environment = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    server.backend.split_pane(SplitPaneOptions {
        target: SplitTarget::Pane(unrelated.clone()),
        command: Some(vec![env.rimz_bin().display().to_string(), "--tmux".into(), "agents".into(),
            "run-command".into(), "--workspace".into(), workspace.workspace_id.to_string(),
            "--run".into(), record.run_id.to_string(), "--".into(), "sh".into(), "-c".into(),
            "printf ready > \"$1\"; read -r line; printf '%s' \"$line\" > \"$2\"; exec sleep 60".into(),
            "command".into(), ready.display().to_string(), received.display().to_string()]),
        cwd: Some(env.project_root.display().to_string()),
        env: environment,
        title: None,
        close_on_exit: false,
        placement: SplitPlacement::default(),
        focus: false,
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(ready.exists(), "a live run must start its command");
    let panes = list_session_panes(&server, &workspace.session_name);
    let own = panes
        .iter()
        .find(|pane| pane.pane_id != unrelated)
        .unwrap()
        .pane_id
        .clone();
    server.tmux(&["send-keys", "-t", own.raw(), "user input", "Enter"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !received.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        std::fs::read_to_string(received).unwrap_or_default(),
        "user input",
        "the command must be able to read its terminal"
    );
    if measure_memory {
        let pid: u32 = server.display(own.raw(), "#{pane_pid}").parse().unwrap();
        let mut candidates = vec![pid];
        let wrapper = loop {
            let pid = candidates.pop().expect("command wrapper process");
            if rimz::proc::argv(pid).is_some_and(|argv| argv.iter().any(|arg| arg == "run-command"))
            {
                break pid;
            }
            candidates.extend(rimz::proc::children(pid));
        };
        let rollup = std::fs::read_to_string(format!("/proc/{wrapper}/smaps_rollup")).unwrap();
        let private_kib: u64 = rollup
            .lines()
            .find_map(|line| {
                line.strip_prefix("Pss_Anon:")
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|value| value.parse().ok())
            })
            .unwrap();
        assert!(
            private_kib < 8 * 1024,
            "command wrapper uses {private_kib} KiB private memory"
        );
    }
    rimz::harness::run::cancel(store.paths(), &record.run_id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let panes = list_session_panes(&server, &workspace.session_name);
        assert!(
            panes.iter().any(|pane| pane.pane_id == unrelated),
            "unrelated pane was closed"
        );
        if panes.iter().all(|pane| pane.pane_id != own) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "terminal run must close even a remain-on-exit pane"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
