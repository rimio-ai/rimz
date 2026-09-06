//! Integration coverage for the agent-facing `rimz wake` doorway.

use crate::common::Env;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::config::Tasks;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::sidebar::refresh::pr::{PrLink, PrStateCache};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::snapshot::{WorktreePrCi, WorktreePrState};
use rimz::store::writer::AgentLifecycleIntent;

#[test]
fn wake_signal_arms_one_shot_instance_for_the_calling_agent() {
    let env = Env::new();
    register_calling_agent(&env);
    let output = agent_wake(&env)
        .args([
            "wake",
            "--signal",
            "deploy.failed",
            "--match",
            "branch=feature",
            "--prompt",
            "inspect CI",
        ])
        .output()
        .expect("arm signal wake");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("armed wake-"), "{stdout}");
    assert!(stdout.contains("on deploy.failed [branch=feature]"));
    assert!(stdout.contains("→ @planner"));

    let tasks: Tasks = serde_json::from_slice(
        &std::fs::read(loop_instances_path(&env)).expect("wake instance store"),
    )
    .expect("wake instances JSON");
    let (name, entry) = tasks.0.iter().next().expect("one wake row");
    assert!(name.starts_with("wake-"));
    assert_eq!(entry.signal.as_deref(), Some("deploy.failed"));
    assert_eq!(
        entry
            .matches
            .as_ref()
            .and_then(|matches| matches.get("branch"))
            .map(String::as_str),
        Some("feature")
    );
    assert_eq!(entry.once, None);
    assert_eq!(entry.timeout.as_deref(), Some("59m"));
    let meta = entry.wake_meta.as_ref().expect("wake provenance");
    assert_eq!(
        entry
            .deadline
            .unwrap()
            .duration_since(meta.armed_at)
            .as_secs(),
        59 * 60
    );
    assert_eq!(entry.prompt.as_deref(), Some("inspect CI"));
    let target = entry.wake.as_ref().expect("pinned wake target");
    assert_eq!(target.kind, "claude");
    assert_eq!(target.session, "provider-session");
    assert_eq!(target.handle, "@planner#project");
}

#[test]
fn calling_agent_can_list_and_cancel_human_armed_wake_by_launch_identity() {
    let env = Env::new();
    register_calling_agent(&env);
    let armed = env
        .rimz()
        .args(["wake", "@planner", "--signal", "deploy.failed"])
        .output()
        .expect("arm wake from human shell");
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let stdout = String::from_utf8_lossy(&armed.stdout);
    let name = stdout
        .strip_prefix("armed ")
        .and_then(|receipt| receipt.split_once(':'))
        .map(|(name, _)| name)
        .expect("armed wake name");

    let listed = agent_wake(&env)
        .args(["wake", "list", "--json"])
        .output()
        .expect("list wake as target agent");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("wake list JSON");
    assert_eq!(rows.as_array().expect("wake rows").len(), 1);
    assert_eq!(rows[0]["name"], name);

    let canceled = agent_wake(&env)
        .args(["wake", "cancel", name])
        .output()
        .expect("cancel wake as target agent");
    assert!(
        canceled.status.success(),
        "{}",
        String::from_utf8_lossy(&canceled.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&canceled.stdout),
        format!("canceled {name}\n")
    );
}

#[test]
fn wake_without_target_refuses_a_plain_shell() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["wake", "--in", "5m"])
        .output()
        .expect("run wake from shell");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("without an explicit @target is only available to an agent"),
        "{stderr}"
    );
}

#[test]
fn wake_rejects_delays_the_minute_scheduler_cannot_represent() {
    let env = Env::new();
    let output = agent_wake(&env)
        .args(["wake", "--in", "1d"])
        .output()
        .expect("run wake with long delay");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--in must be less than 24h"), "{stderr}");
}

#[test]
fn wake_rejects_signal_timeouts_at_or_above_24_hours() {
    let env = Env::new();
    register_calling_agent(&env);
    for timeout in ["24h", "25h"] {
        let output = agent_wake(&env)
            .args(["wake", "--signal", "deploy.failed", "--timeout", timeout])
            .output()
            .expect("reject long signal timeout");
        assert!(!output.status.success(), "accepted --timeout {timeout}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--timeout must be less than 24h"),
            "{stderr}"
        );
    }
}

#[test]
fn wake_wait_reports_a_watched_failure_and_settles_its_message() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let store = env.store();

    let output = agent_wake(&env)
        .args([
            "wake",
            "--wait=5s",
            "--",
            "sh",
            "-c",
            "sleep 1; seq 1 5000; printf watched; exit 3",
        ])
        .output()
        .expect("wait for watched wake");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("delivered · exit 3 after"), "{stdout}");
    assert!(stdout.contains("watched"), "{stdout}");
    let records = wake_records(&env);
    let check = records[0].check.as_ref().unwrap();
    let path = check.output_path.as_ref().expect("watch output path");
    let full = std::fs::read_to_string(path).expect("full watch output");
    assert_eq!(
        full,
        format!(
            "{}watched",
            (1..=5000)
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        )
    );
    assert!(check.output.len() <= 4096);
    assert!(full.ends_with(&check.output));
    assert!(stdout.contains(&path.display().to_string()), "{stdout}");
    let message_id = records[0].message_id.as_ref().unwrap();
    let message = wake_ok(&env, &["message", "show", message_id.as_str()]);
    assert!(message.contains("waited on `"), "{message}");
    assert!(message.contains("exit 3 after"), "{message}");
    assert!(!message.contains("exit 3 after 0s"), "{message}");
    assert!(!message.contains("armed by you"), "{message}");
    assert!(
        message.contains(&format!("output: {}", path.display())),
        "{message}"
    );
    assert!(message.contains("5000\n  watched"), "{message}");
    let logs = wake_ok(&env, &["loop", "logs", &records[0].task]);
    assert!(
        logs.contains(&records[0].watch.as_ref().unwrap().label()),
        "{logs}"
    );
    assert!(logs.contains(&path.display().to_string()), "{logs}");
    let shown = wake_ok(&env, &["loop", "show", &records[0].task]);
    assert!(
        shown.contains(&records[0].watch.as_ref().unwrap().label()),
        "{shown}"
    );
    assert!(!message.contains("--- watch"), "{message}");
    assert!(store.list_pending_messages().unwrap().is_empty());

    let instances: Tasks = serde_json::from_slice(
        &std::fs::read(loop_instances_path(&env)).expect("wake instance store"),
    )
    .expect("wake instances JSON");
    assert!(instances.0.is_empty());
}

#[test]
fn watched_wake_survives_the_arming_process_group_exiting() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let child = agent_wake(&env)
        .args([
            "wake",
            "--json",
            "--",
            "sh",
            "-c",
            "sleep 1; printf survived",
        ])
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let armer_group = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let armed = child.wait_with_output().unwrap();
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let killed = nix::sys::signal::killpg(armer_group, nix::sys::signal::Signal::SIGTERM);
    assert!(killed.is_ok() || killed == Err(nix::errno::Errno::ESRCH));
    let records = wait_for_wake_records(&env, 1);
    assert!(
        matches!(records[0].watch.as_ref().unwrap(), rimz::harness::schedule::signal::WatchVerdict::Exited { code: Some(0), elapsed_ms } if *elapsed_ms >= 1_000)
    );
    let message = env.store().list_pending_messages().unwrap().pop().unwrap();
    assert!(message.text.starts_with("waited on `"), "{}", message.text);
    assert!(message.text.contains("survived"), "{}", message.text);
}

#[test]
fn missing_watcher_row_reports_its_error_to_the_wake_log() {
    let env = Env::new();
    let store = env.store();
    let path = store.paths().wakes_dir.join("wake-missing.log");
    let output = std::fs::File::create(&path).unwrap();
    let status = env
        .rimz()
        .args(["wake", "watch", "wake-missing"])
        .stderr(output)
        .status()
        .unwrap();
    assert!(!status.success());
    let log = std::fs::read_to_string(path).unwrap();
    assert!(
        log.contains("no wake named wake-missing in the catalog"),
        "{log}"
    );
}

#[test]
fn lost_watcher_delivers_elapsed_and_the_existing_log_tail() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let receipt = wake_ok(
        &env,
        &[
            "wake",
            "--json",
            "--",
            "sh",
            "-c",
            "printf started; exec sleep 30",
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let store = env.store();
    let path = store.paths().wakes_dir.join(format!("{name}.log"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let watcher = loop {
        if std::fs::read_to_string(&path).is_ok_and(|output| output == "started") {
            break rimz::harness::schedule::signal::watcher_info(store.runtime_paths(), name)
                .unwrap()
                .unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "watcher did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(watcher.pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    while rimz::harness::schedule::signal::watcher_info(store.runtime_paths(), name)
        .unwrap()
        .is_some()
    {
        assert!(std::time::Instant::now() < deadline, "watcher did not stop");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let armed_at = jiff::Timestamp::now()
        .checked_sub(std::time::Duration::from_secs(60))
        .unwrap();
    let mut tasks = wake_instances(&env);
    tasks
        .0
        .get_mut(name)
        .unwrap()
        .wake_meta
        .as_mut()
        .unwrap()
        .armed_at = armed_at;
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    std::fs::write(
        store.runtime_paths().root.join("loop-fire.json"),
        serde_json::to_vec(&std::collections::BTreeMap::from([(name, armed_at)])).unwrap(),
    )
    .unwrap();
    wake_ok(&env, &["loop", "tick"]);
    let records = wait_for_wake_records(&env, 1);
    let verdict = records[0].watch.as_ref().unwrap();
    assert!(
        matches!(verdict, rimz::harness::schedule::signal::WatchVerdict::Lost { elapsed_ms, .. } if *elapsed_ms >= 60_000)
    );
    assert_eq!(
        records[0].check.as_ref().unwrap().output_path.as_ref(),
        Some(&path)
    );
    let message = store.list_pending_messages().unwrap().pop().unwrap();
    assert!(message.text.contains(&verdict.label()), "{}", message.text);
    assert!(message.text.contains("started"), "{}", message.text);
    let logs = wake_ok(&env, &["loop", "logs", name]);
    assert!(logs.contains(&verdict.label()), "{logs}");
    assert!(logs.contains(&path.display().to_string()), "{logs}");
    let shown = wake_ok(&env, &["loop", "show", &records[0].task]);
    assert!(
        shown.contains(&records[0].watch.as_ref().unwrap().label()),
        "{shown}"
    );
}

#[test]
fn watch_retires_without_delivery_when_its_polarity_does_not_match() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);

    let output = agent_wake(&env)
        .args(["wake", "--on", "fail", "--wait=5s", "--", "true"])
        .output()
        .expect("wait for successful watched command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("skipped · exit 0"), "{stdout}");
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    let instances: Tasks = serde_json::from_slice(
        &std::fs::read(loop_instances_path(&env)).expect("wake instance store"),
    )
    .expect("wake instances JSON");
    assert!(instances.0.is_empty());
}

#[test]
fn signal_wake_siblings_do_not_deliver_or_extend_deadline() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_subscription(&env);
    let deadline = wake_instances(&env).0[&name].deadline;

    wake_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.passed",
            "--json",
            r#"{"branch":"other"}"#,
        ],
    );
    assert!(wake_records(&env).is_empty());
    assert_eq!(wake_instances(&env).0[&name].deadline, deadline);

    wake_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.passed",
            "--json",
            r#"{"branch":"feature"}"#,
        ],
    );
    let records = wait_for_wake_records(&env, 1);
    assert_eq!(records[0].result.label(), "skipped");
    assert!(records[0].message_id.is_none());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    assert_eq!(
        serde_json::to_value(records[0].result).unwrap(),
        "signal_skipped"
    );
    assert_eq!(wake_instances(&env).0[&name].deadline, deadline);
}

#[test]
fn signal_wake_retires_after_one_delivery_and_ignores_repeated_match() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_subscription(&env);

    for fired in [1, 0] {
        let receipt = wake_ok(
            &env,
            &[
                "events",
                "emit",
                "deploy.failed",
                "--json",
                r#"{"branch":"feature","reason":"red"}"#,
            ],
        );
        assert!(
            receipt.contains(&format!("fired {fired} tasks")),
            "{receipt}"
        );
        let records = wait_for_wake_records(&env, 1);
        let record = records.last().unwrap();
        assert_eq!(record.result.label(), "delivered");
        let message_id = record.message_id.as_ref().expect("delivered message");
        let message = wake_ok(&env, &["message", "show", message_id.as_str()]);
        assert!(message.contains("deploy.failed"), "{message}");
        assert!(message.contains("feature"), "{message}");
        assert!(!wake_instances(&env).0.contains_key(&name));
    }
    assert_eq!(wake_records(&env).len(), 1);
    assert_eq!(env.store().list_pending_messages().unwrap().len(), 1);
}

#[test]
fn pending_ci_cache_at_deadline_delivers_status_and_rearm_once() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let worktree = env.project_root.join("feat-x");
    register_calling_agent_in(&env, &worktree, LaunchParams::default());
    let receipt = wake_ok(&env, &["wake", "--signal", "ci.failed", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    write_ci_cache(&env, &worktree, WorktreePrCi::Pending);
    expire_subscription(&env, name);
    wake_ok(&env, &["loop", "tick"]);
    let records = wait_for_wake_records(&env, 1);
    assert_eq!(records[0].result.label(), "expired");
    let message = env.store().list_pending_messages().unwrap().pop().unwrap();
    assert!(
        message.text.contains("ci pending on feat-x (PR #91)"),
        "{}",
        message.text
    );
    assert!(
        message
            .text
            .contains("waited on ci.failed on feat-x (PR #91)")
    );
    assert!(message.text.contains("nothing in 59m; wake closed"));
    assert_eq!(
        rearm_args(&message.text),
        [
            "rimz",
            "wake",
            "--signal",
            "ci.failed",
            "--match",
            &format!("path={}", worktree.display())
        ]
    );
    assert!(!wake_instances(&env).0.contains_key(name));
    wake_ok(&env, &["loop", "run", name, "--expired"]);
    assert_eq!(wake_records(&env).len(), 1);
    assert_eq!(env.store().list_pending_messages().unwrap().len(), 1);
}

#[test]
fn passing_ci_cache_at_deadline_closes_silently_without_a_signal() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let worktree = env.project_root.join("feat-x");
    register_calling_agent_in(&env, &worktree, LaunchParams::default());
    write_ci_cache(&env, &worktree, WorktreePrCi::Passing);
    let receipt = wake_ok(&env, &["wake", "--signal", "ci.failed", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    expire_subscription(&env, name);
    let output = wake_ok(&env, &["loop", "run", name, "--expired"]);
    let records = wake_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].result.label(), "expired");
    assert!(records[0].message_id.is_none(), "{records:?}");
    let signal = records[0].signal.as_ref().expect("durable cached answer");
    assert_eq!(signal.name.as_str(), "ci.passed");
    assert_eq!(signal.payload["path"], worktree.display().to_string());
    assert_eq!(signal.payload["branch"], "feat-x");
    assert_eq!(signal.payload["number"], 91);
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    assert!(
        output.contains("answered · ci passing on feat-x (PR #91)"),
        "{output}"
    );
    assert!(!wake_instances(&env).0.contains_key(name));
    for command in ["logs", "show"] {
        let output = wake_ok(&env, &["loop", command, name]);
        assert!(output.contains("ci.passed"), "{output}");
    }
}

fn write_ci_cache(env: &Env, worktree: &std::path::Path, ci: WorktreePrCi) {
    let mut cache = PrStateCache::default();
    cache.states.insert(
        worktree.display().to_string(),
        PrLink {
            branch: Some("feat-x".to_owned()),
            incarnation: None,
            state: WorktreePrState::Open,
            number: Some(91),
            url: None,
            ci: Some(ci),
            merge_sha: None,
        },
    );
    std::fs::write(
        env.store().runtime_paths().pr_state_path(),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

#[test]
fn custom_signal_wake_expires_with_a_closing_message_and_rearm() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_subscription(&env);
    expire_subscription(&env, &name);
    wake_ok(&env, &["loop", "tick"]);
    let records = wait_for_wake_records(&env, 1);
    assert_eq!(records[0].result.label(), "expired");
    let message_id = records[0].message_id.as_ref().expect("expiry message");
    let message = wake_ok(&env, &["message", "show", message_id.as_str()]);
    assert!(
        message.contains(&format!("wake closed [{name}]")),
        "{message}"
    );
    assert!(
        message.contains("waited on deploy.failed on feature")
            && message.contains("nothing in 59m"),
        "{message}"
    );
    assert_eq!(
        rearm_args(&message),
        [
            "rimz",
            "wake",
            "--signal",
            "deploy.failed",
            "--match",
            "branch=feature"
        ]
    );
    assert!(!wake_instances(&env).0.contains_key(&name));
}

#[test]
fn missing_ci_cache_at_deadline_delivers_unknown_status_and_rearm() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let worktree = env.project_root.join("feat-x");
    register_calling_agent_in(&env, &worktree, LaunchParams::default());
    let receipt = wake_ok(&env, &["wake", "--signal", "ci.failed", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    assert!(!env.store().runtime_paths().pr_state_path().exists());
    expire_subscription(&env, name);
    wake_ok(&env, &["loop", "run", name, "--expired"]);
    let records = wake_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].result.label(), "expired");
    assert!(records[0].message_id.is_some());
    let message = env.store().list_pending_messages().unwrap().pop().unwrap();
    assert!(
        message.text.contains("nothing in 59m; wake closed"),
        "{}",
        message.text
    );
    assert!(
        message.text.contains("no PR or CI seen on"),
        "{}",
        message.text
    );
    assert_eq!(
        rearm_args(&message.text),
        [
            "rimz",
            "wake",
            "--signal",
            "ci.failed",
            "--match",
            &format!("path={}", worktree.display())
        ]
    );
    assert!(!wake_instances(&env).0.contains_key(name));
}

#[test]
fn signal_wake_rearm_replaces_note_and_deadline_and_cancel_wins_lapse() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_subscription(&env);
    let mut tasks = wake_instances(&env);
    let entry = tasks.0.get_mut(&name).unwrap();
    let previous_arm = jiff::Timestamp::UNIX_EPOCH;
    entry.wake_meta.as_mut().unwrap().armed_at = previous_arm;
    entry.prompt = Some("old note".to_owned());
    let previous_deadline = entry.deadline;
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    let receipt = wake_ok(
        &env,
        &[
            "wake",
            "--signal",
            "deploy.failed",
            "--match",
            "branch=feature",
            "--timeout",
            "10m",
            "--prompt",
            "new note",
        ],
    );
    assert!(receipt.starts_with(&format!("armed {name}:")), "{receipt}");
    let tasks = wake_instances(&env);
    assert_eq!(tasks.0.len(), 1);
    let entry = &tasks.0[&name];
    assert_eq!(entry.prompt.as_deref(), Some("new note"));
    assert_eq!(entry.timeout.as_deref(), Some("10m"));
    let armed_at = entry.wake_meta.as_ref().unwrap().armed_at;
    assert!(armed_at > previous_arm);
    assert_ne!(entry.deadline, previous_deadline);
    assert_eq!(
        entry.deadline.unwrap().duration_since(armed_at).as_secs(),
        600
    );
    let listed = wake_ok(&env, &["wake", "list"]);
    assert!(listed.contains("waiting"), "{listed}");
    assert!(!listed.contains("listening"), "{listed}");
    wake_ok(&env, &["loop", "run", &name, "--expired"]);
    assert!(wake_records(&env).is_empty());
    assert!(wake_instances(&env).0.contains_key(&name));
    wake_ok(&env, &["wake", "cancel", &name]);
    wake_ok(&env, &["loop", "run", &name, "--expired"]);
    assert!(wake_records(&env).is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn loop_signal_siblings_preserve_forever_and_once_subscriptions() {
    for once in [false, true] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        let mut args = vec![
            "loop",
            "add",
            "deployment",
            "--wake",
            "@planner",
            "--signal",
            "deploy.failed",
        ];
        if once {
            args.push("--once");
        }
        wake_ok(&env, &args);
        wake_ok(&env, &["events", "emit", "deploy.passed"]);
        let records = wake_records(&env);
        assert_eq!(records.len(), 1, "skip is recorded before emit returns");
        assert_eq!(
            serde_json::to_value(records[0].result).unwrap(),
            "signal_skipped"
        );
        assert_eq!(
            records[0].signal.as_ref().unwrap().name.as_str(),
            "deploy.passed"
        );
        assert!(env.store().list_pending_messages().unwrap().is_empty());
        wake_ok(&env, &["loop", "show", "deployment"]);
        wake_ok(&env, &["events", "emit", "deploy.failed"]);
        let records = wait_for_wake_records(&env, 2);
        assert_eq!(records[1].result.label(), "delivered");
        if once {
            assert!(!wake_instances(&env).0.contains_key("deployment"));
        } else {
            wake_ok(&env, &["loop", "show", "deployment"]);
            wake_ok(&env, &["events", "emit", "deploy.failed"]);
            assert_eq!(
                wait_for_wake_records(&env, 3)[2].result.label(),
                "delivered"
            );
        }
    }
}

fn wake_ok(env: &Env, args: &[&str]) -> String {
    let output = agent_wake(env)
        .args(args)
        .output()
        .expect("run wake command");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn arm_subscription(env: &Env) -> String {
    let receipt = wake_ok(
        env,
        &[
            "wake",
            "--signal",
            "deploy.failed",
            "--match",
            "branch=feature",
            "--json",
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    receipt["name"].as_str().unwrap().to_owned()
}

fn rearm_args(body: &str) -> Vec<String> {
    let command = body
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("re-arm: "))
        .expect("closing message re-arm command");
    shlex::split(command).expect("shell-safe re-arm command")
}

fn loop_instances_path(env: &Env) -> std::path::PathBuf {
    env.state_root().join("rimz").join("loop-instances.json")
}

fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.state_root().join("rimz").join("loop-runs.log.jsonl")
}

fn wake_instances(env: &Env) -> Tasks {
    let path = loop_instances_path(env);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn expire_subscription(env: &Env, name: &str) {
    let mut tasks = wake_instances(env);
    tasks.0.get_mut(name).unwrap().deadline = Some(jiff::Timestamp::UNIX_EPOCH);
    let path = loop_instances_path(env);
    std::fs::write(path, serde_json::to_vec(&tasks).unwrap()).unwrap();
}

fn wake_records(env: &Env) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let path = loop_runs_path(env);
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn wait_for_wake_records(
    env: &Env,
    count: usize,
) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let records = wake_records(env);
        if records.len() >= count {
            return records;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected {count} records, got {records:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[test]
fn wake_defaults_scope_from_caller_worktree() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let worktree = env.project_root.join("feature");
    register_calling_agent_in(&env, &worktree, LaunchParams::default());
    wake_ok(&env, &["wake", "--signal", "ci.*"]);
    let tasks = wake_instances(&env);
    let entry = tasks.0.values().next().unwrap();
    assert_eq!(
        entry.matches.as_ref().unwrap()["path"],
        worktree.display().to_string()
    );
    let output = env
        .rimz()
        .args(["wake", "@planner", "--signal", "pr.merged"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        wake_instances(&env)
            .0
            .values()
            .all(|entry| entry.matches.as_ref().unwrap()["path"] == worktree.display().to_string())
    );
    wake_ok(
        &env,
        &[
            "loop",
            "add",
            "ci-loop",
            "--wake",
            "@planner",
            "--signal",
            "ci.failed",
            "--prompt",
            "inspect CI",
        ],
    );
    let listed = wake_ok(&env, &["loop", "show", "ci-loop"]);
    assert!(
        listed.contains(&format!("path={}", worktree.display())),
        "{listed}"
    );
}

#[test]
fn wake_on_ci_from_root_checkout_refuses_with_fixes() {
    let env = Env::new();
    register_calling_agent(&env);
    let output = agent_wake(&env)
        .args(["wake", "--signal", "ci.failed"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("root checkout is not watched"), "{error}");
    assert!(error.contains("--match branch=<name>"), "{error}");
    assert!(
        error.contains("rimz wake -- gh run watch --exit-status"),
        "{error}"
    );
    wake_ok(
        &env,
        &["wake", "--signal", "ci.failed", "--match", "branch=feature"],
    );
    let output = agent_wake(&env)
        .args(["wake", "--signal", "ci.finished"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("was replaced by ci.passed, ci.failed")
    );
}

#[test]
fn wake_on_team_defaults_to_own_instance() {
    let env = Env::new();
    register_calling_agent_in(
        &env,
        &env.project_root,
        LaunchParams {
            team: Some("forge".to_owned()),
            channel: Some("feature".to_owned()),
            ..LaunchParams::default()
        },
    );
    wake_ok(&env, &["wake", "--signal", "team.idle"]);
    let tasks = wake_instances(&env);
    assert_eq!(
        tasks.0.values().next().unwrap().matches.as_ref().unwrap()["instance"],
        "forge#feature"
    );
}

fn register_calling_agent(env: &Env) {
    register_calling_agent_in(env, &env.project_root, LaunchParams::default());
}

fn register_calling_agent_in(env: &Env, worktree: &std::path::Path, launch: LaunchParams) {
    register_agent_in(
        env,
        worktree,
        launch,
        "planner",
        "provider-session",
        "launch-session",
    );
}

#[test]
fn lifecycle_hooks_deliver_team_idle_and_root_ended_signals() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    register_agent_in(
        &env,
        &env.project_root,
        LaunchParams {
            team: Some("forge".to_owned()),
            channel: Some("feature".to_owned()),
            ..LaunchParams::default()
        },
        "coder",
        "worker-session",
        "worker-launch",
    );
    for (signal, filter) in [
        ("team.idle", "instance=forge#feature"),
        ("team.ended", "instance=forge#feature"),
        ("agent.ended", "session=worker-session"),
    ] {
        wake_ok(&env, &["wake", "--signal", signal, "--match", filter]);
    }
    for event in ["UserPromptSubmit", "Stop", "SessionEnd"] {
        let mut command = env.hook_command("claude");
        command
            .env("RIMZ_AGENT_PID", std::process::id().to_string())
            .env("RIMZ_CHANNEL", "feature")
            .env("RIMZ_AGENT_NAME", "coder");
        let payload = serde_json::json!({
            "hook_event_name": event,
            "session_id": "worker-session",
            "prompt": "finish work",
            "last_assistant_message": "work complete",
            "reason": "other"
        })
        .to_string();
        let output = env
            .spawn_payload(command, &payload)
            .wait_with_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if event == "Stop" {
            let records = wait_for_wake_records(&env, 3);
            assert!(records.iter().any(|record| {
                record.result.label() == "delivered"
                    && record
                        .signal
                        .as_ref()
                        .is_some_and(|signal| signal.name.as_str() == "team.idle")
            }));
        }
    }
    let records = wait_for_wake_records(&env, 5);
    for signal in ["team.idle", "team.ended", "agent.ended"] {
        let record = records
            .iter()
            .find(|record| {
                record.result.label() == "delivered"
                    && record
                        .signal
                        .as_ref()
                        .is_some_and(|value| value.name.as_str() == signal)
            })
            .expect("delivered lifecycle wake");
        let message = wake_ok(
            &env,
            &[
                "message",
                "show",
                record.message_id.as_ref().unwrap().as_str(),
            ],
        );
        assert!(message.contains(signal), "{message}");
    }
}

fn register_agent_in(
    env: &Env,
    worktree: &std::path::Path,
    launch: LaunchParams,
    name: &str,
    session: &str,
    launch_id: &str,
) {
    let store = env.store();
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace resolves");
    store
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from(session),
                launch_id: Some(AgentSessionId::from(launch_id)),
                agent_name: name.to_owned(),
                agent_name_explicit: true,
                launch,
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(worktree.display().to_string()),
                worktree_branch: Some(name.to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched target");
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session)),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some(name.to_owned());
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("register target");
}

fn agent_wake(env: &Env) -> std::process::Command {
    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("RIMZ_AGENT_NAME", "planner");
    command
}
