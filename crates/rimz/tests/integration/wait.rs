//! Integration coverage for the agent-facing `rimz wait` doorway.

use crate::common::{Env, canonical};
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::config::Tasks;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use rimz::store::writer::AgentLifecycleIntent;

#[test]
fn wait_delay_arms_instance_for_the_calling_agent() {
    let env = Env::new();
    register_calling_agent_with_launch(
        &env,
        LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("planner".to_owned()),
            ..LaunchParams::default()
        },
    );
    let stdout = wait_ok(&env, &["wait", "--in", "5m"]);
    assert!(stdout.starts_with("armed wait-"), "{stdout}");
    assert!(stdout.contains("in 5m"), "{stdout}");
    assert!(stdout.contains("→ @planner"), "{stdout}");
    let tasks = wait_instances(&env);
    assert_eq!(tasks.0.len(), 1);
    let (name, entry) = tasks.0.iter().next().unwrap();
    assert!(name.starts_with("wait-"));
    assert!(entry.at.is_some());
    assert!(entry.signal.is_none());
    assert!(entry.deadline.is_none());
    assert!(entry.prompt.is_none());
    assert!(entry.wait_meta.is_some());
    let target = entry.wait.as_ref().expect("pinned wait target");
    assert_eq!(target.kind.as_str(), "claude");
    assert_eq!(target.session.as_str(), "provider-session");
    assert_eq!(target.handle, "@planner#project");

    for signal in [
        LifecycleSignal::TurnStarted,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    ] {
        let observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from("provider-session")), signal);
        env.store()
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .expect("record the arming turn");
    }
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report");
    assert_eq!(report["agent"]["status"], "sleeping");
    assert_eq!(report["agent"]["pending_waits"][0]["name"], *name);
    let teams: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["teams", "--json"])).expect("team report");
    assert_eq!(teams[0]["instances"][0]["state"], "sleeping", "{teams}");
    wait_ok(&env, &["wait", "cancel", name]);
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report after cancellation");
    assert_eq!(report["agent"]["status"], "success");
    let teams: serde_json::Value = serde_json::from_str(&wait_ok(&env, &["teams", "--json"]))
        .expect("team report after cancellation");
    assert_eq!(teams[0]["instances"][0]["state"], "done");
}

#[test]
fn calling_agent_can_list_and_cancel_human_armed_loop_delivery_by_launch_identity() {
    let env = Env::new();
    register_calling_agent(&env);
    let armed = env
        .rimz()
        .args([
            "loop",
            "add",
            "deployment",
            "--wait",
            "@planner",
            "--signal",
            "deploy.failed",
        ])
        .output()
        .expect("arm wait from human shell");
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let name = "deployment";

    let listed = agent_wait(&env)
        .args(["wait", "list", "--json"])
        .output()
        .expect("list wait as target agent");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("wait list JSON");
    assert_eq!(rows.as_array().expect("wait rows").len(), 1);
    assert_eq!(rows[0]["name"], name);
    assert!(rows[0].get("dir").is_none());

    let canceled = agent_wait(&env)
        .args(["wait", "cancel", name])
        .output()
        .expect("cancel wait as target agent");
    assert!(
        canceled.status.success(),
        "{}",
        String::from_utf8_lossy(&canceled.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&canceled.stdout),
        format!("canceled {name}\nno pending waits\n")
    );
}

#[test]
fn wait_arm_refuses_a_plain_shell() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["wait", "--in", "5m"])
        .output()
        .expect("run wait from shell");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("arming a wait is only available to an agent"),
        "{stderr}"
    );
}

#[test]
fn wait_rejects_delays_the_minute_scheduler_cannot_represent() {
    let env = Env::new();
    let output = agent_wait(&env)
        .args(["wait", "--in", "1d"])
        .output()
        .expect("run wait with long delay");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--in must be less than 24h"), "{stderr}");
}

#[test]
fn wait_rejects_watch_checkins_at_or_above_24_hours() {
    let env = Env::new();
    register_calling_agent(&env);
    for timeout in ["24h", "25h"] {
        let output = agent_wait(&env)
            .args(["wait", "--timeout", timeout, "--", "true"])
            .output()
            .expect("reject long watch check-in");
        assert!(!output.status.success(), "accepted --timeout {timeout}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--timeout must be less than 24h"),
            "{stderr}"
        );
    }
}

#[test]
fn wait_pid_checks_in_then_delivers_after_process_disappears_with_empty_summary() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let mut process = std::process::Command::new("sleep")
        .arg("30")
        .env("HOME", &env.home_root)
        .spawn()
        .unwrap();
    let pid = process.id().to_string();
    let receipt = wait_ok(&env, &["wait", "--pid", &pid, "--timeout", "1s", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["trigger"], format!("pid {pid}"));
    assert_eq!(receipt["trigger"], receipt["pending"][0]["trigger"]);
    let listed: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(listed[0]["trigger"], receipt["trigger"]);
    let tasks = wait_instances(&env);
    let entry = &tasks.0[receipt["name"].as_str().unwrap()];
    assert_eq!(entry.wait_meta.as_ref().unwrap().pid, Some(process.id()));
    assert_eq!(entry.on, Some(rimz::config::CheckOn::Any));
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report");
    assert_eq!(
        report["agent"]["pending_waits"][0]["trigger"],
        serde_json::json!({"kind": "pid", "pid": process.id()})
    );
    let checkin = wait_for_wait_messages(&env, 1);
    assert!(checkin[0].text.contains("still running after"));
    assert!(checkin[0].text.contains("output (0 B, 0 lines):"));
    assert!(!checkin[0].text.contains("(no output)"));
    assert!(process.try_wait().unwrap().is_none());
    assert_eq!(wait_instances(&env).0.len(), 1);
    process.kill().unwrap();
    process.wait().unwrap();
    let messages = wait_for_wait_messages(&env, 2);
    let completed = messages
        .iter()
        .find(|message| message.text.contains("exit 0 after"))
        .expect("process disappearance delivered");
    assert!(completed.text.contains(&pid));
    assert!(completed.text.contains("output (0 B, 0 lines):"));
    assert!(!completed.text.contains("(no output)"));
    wait_for_no_wait_instances(&env);

    let receipt = wait_ok(&env, &["wait", "--pid", &pid]);
    assert!(receipt.starts_with("armed wait-"), "{receipt}");
    assert!(receipt.contains(&format!(": pid {pid} →")), "{receipt}");
    assert_eq!(wait_for_wait_messages(&env, 3).len(), 3);
    wait_for_no_wait_instances(&env);
}

#[test]
fn canceling_pid_wait_leaves_the_existing_process_running() {
    let env = Env::new();
    register_calling_agent(&env);
    let mut process = std::process::Command::new("sleep")
        .arg("30")
        .env("HOME", &env.home_root)
        .spawn()
        .unwrap();
    let receipt = wait_ok(
        &env,
        &["wait", "--pid", &process.id().to_string(), "--json"],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    wait_until("process watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_some()
    });
    wait_ok(&env, &["wait", "cancel", name]);
    assert!(process.try_wait().unwrap().is_none());
    assert!(wait_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    process.kill().unwrap();
    process.wait().unwrap();
}

#[test]
fn wait_pid_refuses_a_process_it_cannot_observe() {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(1), None) != Err(nix::errno::Errno::EPERM)
    {
        return;
    }
    let env = Env::new();
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .args(["wait", "--pid", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot watch PID 1: permission denied"),
        "{stderr}"
    );
    assert!(
        stderr.contains("choose a process owned by your user"),
        "{stderr}"
    );
    assert!(!loop_instances_path(&env).exists());
}

#[test]
fn wait_pid_rejects_invalid_pids_and_conflicting_triggers() {
    let env = Env::new();
    for args in [
        vec!["wait", "--pid", "0"],
        vec!["wait", "--pid=-1"],
        vec!["wait", "--pid", "2147483648"],
        vec!["wait", "--pid", "not-a-pid"],
        vec!["wait", "--pid", "123;true"],
        vec!["wait", "--pid", "123", "--in", "5m"],
        vec!["wait", "--pid", "123", "--", "true"],
        vec!["wait", "--pid", "123", "--on", "success"],
    ] {
        let output = agent_wait(&env).args(&args).output().unwrap();
        assert!(!output.status.success(), "accepted {args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("invalid value")
                || stderr.contains("choose exactly one wait trigger")
                || stderr.contains("--on requires a command"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn watched_wait_runs_in_the_arming_worktree() {
    let env = Env::new();
    let initialized = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&env.project_root)
        .status();
    if !initialized.is_ok_and(|status| status.success()) {
        tracing::warn!("skipping: git unavailable");
        return;
    }
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=rimz@example.invalid",
            "-c",
            "user.name=RimZ",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ])
        .current_dir(&env.project_root)
        .status()
        .expect("run git commit");
    assert!(commit.success(), "git commit failed");
    let linked = env.home_root.join("linked");
    let add = std::process::Command::new("git")
        .args(["worktree", "add", "-q", "-b", "linked"])
        .arg(&linked)
        .current_dir(&env.project_root)
        .status()
        .expect("run git worktree add");
    assert!(add.success(), "git worktree add failed");
    let linked = canonical(&linked);
    let release = env.home_root.join("release-watch");
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .current_dir(&linked)
        .args([
            "wait",
            "--json",
            "--",
            "sh",
            "-c",
            "while [ ! -e \"$1\" ]; do sleep 0.025; done; pwd -P",
            "watch-cwd",
        ])
        .arg(&release)
        .output()
        .expect("arm watched wait from linked worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let tasks = wait_instances(&env);
    let entry = &tasks.0[name];
    assert_eq!(entry.root, canonical(&env.project_root));
    assert_eq!(entry.dir.as_deref(), Some(linked.as_path()));
    let listed: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(listed[0]["name"], name);
    assert_eq!(listed[0]["dir"], "~/linked");

    std::fs::write(&release, "").expect("release watched command");
    let records = wait_for_wait_records(&env, 1);
    assert_eq!(records[0].root, Some(canonical(&env.project_root)));
    let check = records[0].check.as_ref().unwrap();
    assert_eq!(check.code, Some(0));
    assert_eq!(check.output.trim(), linked.to_str().unwrap());
    wait_for_no_wait_instances(&env);
}

#[test]
fn watched_failure_preserves_full_output_and_delivers_its_summary() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .args([
            "wait",
            "--json",
            "--",
            "sh",
            "-c",
            "sleep 1; seq 1 5000; printf watched; exit 3",
        ])
        .output()
        .expect("arm watched wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = wait_for_wait_records(&env, 1);
    let check = records[0].check.as_ref().unwrap();
    let path = check.output_path.as_ref().expect("watch output path");
    assert_eq!(
        path,
        &env.store()
            .paths()
            .waits_dir
            .join(format!("{}.output", records[0].task))
    );
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
    let message_id = records[0].message_id.as_ref().unwrap();
    let message = wait_ok(&env, &["message", "show", message_id.as_str()]);
    assert!(message.contains("waited on `"), "{message}");
    assert!(message.contains("exit 3 after"), "{message}");
    assert!(!message.contains("exit 3 after 0s"), "{message}");
    assert!(!message.contains("armed by you"), "{message}");
    assert!(
        message.contains(&format!(
            "output ({}, 5001 lines): {}",
            rimz::theme::fmt::fmt_bytes(full.len() as u64),
            path.display()
        )),
        "{message}"
    );
    assert!(!message.contains("5000\n"), "{message}");
    assert!(!message.contains(&check.output), "{message}");
    let logs = wait_ok(&env, &["loop", "logs", &records[0].task]);
    assert!(
        logs.contains(&records[0].watch.as_ref().unwrap().label()),
        "{logs}"
    );
    assert!(logs.contains(&path.display().to_string()), "{logs}");
    let shown = wait_ok(&env, &["loop", "show", &records[0].task]);
    assert!(
        shown.contains(&records[0].watch.as_ref().unwrap().label()),
        "{shown}"
    );
    assert!(!message.contains("--- watch"), "{message}");
    assert_eq!(wait_for_wait_messages(&env, 1).len(), 1);

    wait_for_no_wait_instances(&env);
}

#[test]
fn watched_wait_survives_the_arming_process_group_exiting() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let child = agent_wait(&env)
        .args([
            "wait",
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
    let records = wait_for_wait_records(&env, 1);
    assert!(
        matches!(records[0].watch.as_ref().unwrap(), rimz::harness::schedule::signal::WatchVerdict::Exited { code: Some(0), elapsed_ms } if *elapsed_ms >= 1_000)
    );
    let message = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert!(message.text.starts_with("waited on `"), "{}", message.text);
    assert!(message.text.contains("survived"), "{}", message.text);
}

#[test]
fn missing_watcher_row_reports_its_error_to_the_wait_output() {
    let env = Env::new();
    let store = env.store();
    store.paths().ensure_tmp_dir().unwrap();
    let path = store.paths().waits_dir.join("wait-missing.output");
    let output = std::fs::File::create(&path).unwrap();
    let status = env
        .rimz()
        .args(["wait", "watch", "wait-missing"])
        .stderr(output)
        .status()
        .unwrap();
    assert!(!status.success());
    let log = std::fs::read_to_string(path).unwrap();
    assert!(
        log.contains("no wait named wait-missing in the catalog"),
        "{log}"
    );
}

#[test]
fn lost_watcher_delivers_elapsed_and_the_existing_output_summary() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let receipt = wait_ok(
        &env,
        &[
            "wait",
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
    let path = store.paths().waits_dir.join(format!("{name}.output"));
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
    let mut tasks = wait_instances(&env);
    tasks
        .0
        .get_mut(name)
        .unwrap()
        .wait_meta
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
    wait_ok(&env, &["loop", "tick"]);
    let records = wait_for_wait_records(&env, 1);
    let verdict = records[0].watch.as_ref().unwrap();
    assert!(
        matches!(verdict, rimz::harness::schedule::signal::WatchVerdict::Lost { elapsed_ms, .. } if *elapsed_ms >= 60_000)
    );
    assert_eq!(
        records[0].check.as_ref().unwrap().output_path.as_ref(),
        Some(&path)
    );
    let message = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert!(message.text.contains(&verdict.label()), "{}", message.text);
    assert!(
        message.text.contains("output (7 B, 1 line):"),
        "{}",
        message.text
    );
    assert_eq!(records[0].check.as_ref().unwrap().output, "started");
    assert!(!message.text.lines().any(|line| line == "started"));
    let logs = wait_ok(&env, &["loop", "logs", name]);
    assert!(logs.contains(&verdict.label()), "{logs}");
    assert!(logs.contains(&path.display().to_string()), "{logs}");
    let shown = wait_ok(&env, &["loop", "show", &records[0].task]);
    assert!(
        shown.contains(&records[0].watch.as_ref().unwrap().label()),
        "{shown}"
    );
}

#[test]
fn watch_retires_without_delivery_when_its_polarity_does_not_match() {
    for (on, command) in [("fail", "true"), ("success", "false")] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        wait_ok(&env, &["wait", "--on", on, "--", command]);
        let records = wait_for_wait_records(&env, 1);
        assert_eq!(records[0].result.label(), "skipped");
        assert!(records[0].message_id.is_none());
        assert!(env.store().list_pending_messages().unwrap().is_empty());
        wait_for_no_wait_instances(&env);
    }
}

#[test]
fn self_wait_queues_with_any_gate_for_working_and_idle_targets() {
    for working in [false, true] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        if working {
            let observation = AgentLifecycleObservation::new(
                Some(AgentSessionId::from("provider-session")),
                LifecycleSignal::TurnStarted,
            );
            env.store()
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: AgentKind::new_unchecked("claude"),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        wait_ok(&env, &["wait", "--", "printf", "self-wait-marker"]);
        let messages = wait_for_wait_messages(&env, 1);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent_id.as_str(), "provider-session");
        assert_eq!(messages[0].gate, DeliveryGate::Any);
        assert_eq!(
            messages[0].sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wait
            }
        );
        assert!(messages[0].text.contains("self-wait-marker"));
        wait_for_no_wait_instances(&env);
    }
}

#[test]
fn watch_checkin_delivers_once_without_consuming_or_killing_command() {
    for (on, exit, delivers_exit) in [
        ("any", "0", true),
        ("success", "0", true),
        ("fail", "3", true),
        ("success", "3", false),
        ("fail", "0", false),
    ] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        let release = env.home_root.join("release");
        let pid_path = env.home_root.join("command.pid");
        let receipt = wait_ok(
            &env,
            &[
                "wait",
                "--json",
                "--on",
                on,
                "--timeout",
                "1s",
                "--",
                "sh",
                "-c",
                "printf '%s' \"$$\" > \"$1\"; printf checkin-marker; while [ ! -e \"$2\" ]; do sleep 0.05; done; printf final-marker; exit \"$3\"",
                "checkin",
                pid_path.to_str().unwrap(),
                release.to_str().unwrap(),
                exit,
            ],
        );
        let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        let name = receipt["name"].as_str().unwrap();
        let messages = wait_for_wait_messages(&env, 1);
        let notice = &messages[0];
        assert!(
            notice.text.contains("still running after"),
            "--on {on}: {}",
            notice.text
        );
        assert!(
            notice.text.contains("output (14 B, 1 line):"),
            "{}",
            notice.text
        );
        assert!(!notice.text.lines().any(|line| line == "checkin-marker"));
        assert!(
            notice.text.contains(&format!("rimz wait cancel {name}")),
            "{}",
            notice.text
        );
        assert!(notice.text.contains("rimz wait --in 1s"), "{}", notice.text);
        assert_eq!(notice.agent_id.as_str(), "provider-session");
        assert_eq!(
            notice.sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wait
            }
        );
        assert_eq!(notice.gate, DeliveryGate::Any);
        let report: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
                .unwrap();
        assert_eq!(report["agent"]["status"], "sleeping");
        assert_eq!(
            report["agent"]["pending_waits"][0]["trigger"]["kind"],
            "command"
        );
        let pid: u32 = std::fs::read_to_string(&pid_path).unwrap().parse().unwrap();
        let observe_until = std::time::Instant::now() + std::time::Duration::from_millis(1200);
        while std::time::Instant::now() < observe_until {
            assert!(
                rimz::proc::process_is_live(pid, None),
                "check-in killed the command"
            );
            assert!(
                wait_instances(&env).0.contains_key(name),
                "check-in consumed the instance"
            );
            assert_eq!(
                env.store().list_pending_messages().unwrap().len(),
                1,
                "duplicate check-in"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let records = wait_for_wait_records(&env, 1);
        assert_eq!(records.len(), 1);
        let check = records[0].check.as_ref().unwrap();
        assert!(!check.timed_out);
        assert_eq!(check.code, None);
        assert!(
            notice
                .text
                .contains(&check.output_path.as_ref().unwrap().display().to_string())
        );

        std::fs::write(&release, "").unwrap();
        let records = wait_for_wait_records(&env, 2);
        assert_eq!(records.len(), 2);
        assert!(matches!(
            records[1].watch.as_ref().unwrap(),
            rimz::harness::schedule::signal::WatchVerdict::Exited { code: Some(code), .. }
                if code.to_string() == exit
        ));
        wait_for_no_wait_instances(&env);
        let report: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
                .unwrap();
        assert_eq!(report["agent"]["status"], "idle");
        assert_eq!(report["agent"]["pending_waits"], serde_json::json!([]));
        let messages = wait_for_wait_messages(&env, if delivers_exit { 2 } else { 1 });
        assert_eq!(messages.len(), if delivers_exit { 2 } else { 1 });
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.text.contains("still running after"))
                .count(),
            1
        );
        if delivers_exit {
            let final_message = messages
                .iter()
                .find(|message| message.message_id != notice.message_id)
                .unwrap();
            assert!(
                final_message.text.contains(&format!("exit {exit} after")),
                "{}",
                final_message.text
            );
            assert!(
                final_message.text.contains("output (26 B, 1 line):"),
                "{}",
                final_message.text
            );
            assert!(
                !final_message
                    .text
                    .lines()
                    .any(|line| line == "checkin-markerfinal-marker")
            );
            assert_eq!(
                records[1].message_id.as_ref(),
                Some(&final_message.message_id)
            );
        } else {
            assert_eq!(records[1].result.label(), "skipped");
            assert!(records[1].message_id.is_none());
        }
    }
}

#[test]
fn once_wait_subscriber_is_consumed_by_watcher_checkin() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    wait_ok(
        &env,
        &[
            "loop", "add", "audit", "--signal", "wait.*", "--wait", "@me", "--once",
        ],
    );
    let release = env.home_root.join("release");
    let receipt = wait_ok(
        &env,
        &[
            "wait",
            "--json",
            "--timeout",
            "1s",
            "--",
            "sh",
            "-c",
            "printf checkin-marker; while [ ! -e \"$1\" ]; do sleep 0.05; done; printf final-marker",
            "checkin",
            release.to_str().unwrap(),
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let records = wait_for_wait_records(&env, 2);
    assert_eq!(records.len(), 2);
    let audit = records
        .iter()
        .find(|record| record.task == "audit")
        .unwrap();
    assert!(matches!(
        audit.watch.as_ref().unwrap(),
        rimz::harness::schedule::signal::WatchVerdict::Running { .. }
    ));
    let messages = wait_for_wait_messages(&env, 2);
    let notice = messages
        .iter()
        .find(|message| Some(&message.message_id) == audit.message_id.as_ref())
        .expect("subscriber check-in reached the durable message consumer");
    assert_eq!(
        notice.sender,
        MessageSender::Harness {
            notice: HarnessNotice::Signal
        }
    );
    let tasks = wait_instances(&env);
    assert!(
        tasks.0.contains_key(name),
        "check-in consumed the watched task"
    );
    assert!(
        !tasks.0.contains_key("audit"),
        "check-in did not consume the once subscriber"
    );

    std::fs::write(&release, "").unwrap();
    wait_for_no_wait_instances(&env);
    wait_until("watcher did not finish its exit delivery", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_none()
    });
    let records = wait_for_wait_records(&env, 3);
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .filter(|record| record.task == "audit")
            .count(),
        1
    );
    assert!(records.iter().any(|record| {
        record.task == name
            && matches!(
                record.watch.as_ref(),
                Some(rimz::harness::schedule::signal::WatchVerdict::Exited { code: Some(0), .. })
            )
    }));
    assert_eq!(wait_for_wait_messages(&env, 3).len(), 3);
}

#[test]
fn wait_cancel_before_watcher_start_prevents_command() {
    let env = Env::new();
    register_calling_agent(&env);
    let receipt = wait_ok(&env, &["wait", "--in", "5m", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let mut tasks = wait_instances(&env);
    let entry = tasks.0.get_mut(name).unwrap();
    entry.at = None;
    entry.watch = Some("touch command-started".to_owned());
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();

    wait_ok(&env, &["wait", "cancel", name]);
    let output = env.rimz().args(["wait", "watch", name]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no wait named"));
    assert!(!env.project_root.join("command-started").exists());
    assert!(wait_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn wait_cancel_all_stops_command_groups_and_prints_pending() {
    let env = Env::new();
    register_calling_agent(&env);
    let mut pids = Vec::new();
    for index in 0..2 {
        let path = env.home_root.join(format!("command-{index}.pids"));
        wait_ok(
            &env,
            &[
                "wait",
                "--",
                "sh",
                "-c",
                "sleep 30 & printf '%s %s' \"$$\" \"$!\" > \"$1\"; wait",
                "cancel",
                path.to_str().unwrap(),
            ],
        );
        wait_until("command group did not start", || {
            std::fs::read_to_string(&path).is_ok_and(|text| text.split_whitespace().count() == 2)
        });
        let command_pids = std::fs::read_to_string(&path).unwrap();
        for pid in command_pids.split_whitespace() {
            let pid = pid.parse::<u32>().unwrap();
            assert!(rimz::proc::process_is_live(pid, None));
            pids.push(pid);
        }
    }
    let names = wait_instances(&env).0.into_keys().collect::<Vec<_>>();
    let canceled = wait_ok(&env, &["wait", "cancel", "--all"]);
    assert!(canceled.starts_with("canceled "), "{canceled}");
    for name in names {
        assert!(canceled.contains(&name), "{canceled}");
    }
    assert!(canceled.ends_with("no pending waits\n"), "{canceled}");
    wait_until("cancel left watched descendants alive", || {
        pids.iter()
            .all(|pid| !rimz::proc::process_is_live(*pid, None))
    });
    assert!(wait_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn wait_receipts_and_list_share_pending_rows() {
    let env = Env::new();
    register_calling_agent(&env);
    let first = wait_ok(&env, &["wait", "--in", "5m", "--json"]);
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    let second = wait_ok(&env, &["wait", "--in", "10m", "--json"]);
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    let listed = wait_ok(&env, &["wait", "list", "--json"]);
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(first["pending"].as_array().unwrap().len(), 1);
    assert_eq!(second["pending"].as_array().unwrap().len(), 2);
    assert_eq!(second["pending"], listed);
    wait_ok(&env, &["loop", "disable", first["name"].as_str().unwrap()]);
    wait_ok(
        &env,
        &[
            "loop",
            "pause",
            second["name"].as_str().unwrap(),
            "--for",
            "1h",
        ],
    );
    let held = wait_ok(&env, &["wait", "list", "--json"]);
    let held: serde_json::Value = serde_json::from_str(&held).unwrap();
    let held = held.as_array().unwrap();
    assert_eq!(
        held.iter()
            .find(|row| row["name"] == first["name"])
            .unwrap()["state"],
        "disabled"
    );
    assert!(
        held.iter()
            .find(|row| row["name"] == second["name"])
            .unwrap()["state"]
            .as_str()
            .unwrap()
            .starts_with("paused")
    );
    let canceled = wait_ok(
        &env,
        &["wait", "cancel", first["name"].as_str().unwrap(), "--json"],
    );
    let canceled: serde_json::Value = serde_json::from_str(&canceled).unwrap();
    assert_eq!(canceled["canceled"], serde_json::json!([first["name"]]));
    assert_eq!(canceled["pending"].as_array().unwrap().len(), 1);
    assert_eq!(canceled["pending"][0]["name"], second["name"]);
    let listed = wait_ok(&env, &["wait", "list", "--json"]);
    assert_eq!(
        canceled["pending"],
        serde_json::from_str::<serde_json::Value>(&listed).unwrap()
    );
    let human = wait_ok(&env, &["wait", "--in", "15m"]);
    assert!(human.starts_with("armed wait-"), "{human}");
    assert!(human.contains(second["name"].as_str().unwrap()), "{human}");
}

#[test]
fn wait_rejects_removed_target_prompt_and_signal_flags() {
    let env = Env::new();
    register_calling_agent(&env);
    for args in [
        vec!["wait", "@planner", "--in", "5m"],
        vec!["wait", "--in", "5m", "--prompt", "note"],
        vec!["wait", "--in", "5m", "--prompt-file", "note.txt"],
        vec!["wait", "--signal", "deploy.failed"],
        vec!["wait", "--in", "5m", "--match", "branch=feature"],
        vec!["wait", "--wait=5s", "--", "true"],
    ] {
        let output = agent_wait(&env).args(&args).output().unwrap();
        assert!(
            !output.status.success(),
            "accepted removed arguments: {args:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unexpected argument"), "{args:?}: {stderr}");
    }
}

fn wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "{description}");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn wait_for_wait_messages(env: &Env, count: usize) -> Vec<MessageRecord> {
    let mut messages = Vec::new();
    wait_until("expected durable wait message at the consumer", || {
        messages = env.store().list_pending_messages().unwrap();
        messages.len() >= count
    });
    messages
}

fn wait_for_no_wait_instances(env: &Env) {
    wait_until("wait instance was not retired", || {
        wait_instances(env).0.is_empty()
    });
}

fn wait_ok(env: &Env, args: &[&str]) -> String {
    let output = agent_wait(env)
        .args(args)
        .output()
        .expect("run wait command");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn loop_instances_path(env: &Env) -> std::path::PathBuf {
    env.state_path_for(&env.project_root)
        .root
        .join("loop-instances.json")
}

fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.state_root().join("rimz").join("loop-runs.log.jsonl")
}

fn wait_instances(env: &Env) -> Tasks {
    let path = loop_instances_path(env);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn wait_records(env: &Env) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let path = loop_runs_path(env);
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn wait_for_wait_records(
    env: &Env,
    count: usize,
) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let records = wait_records(env);
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

fn register_calling_agent(env: &Env) {
    register_calling_agent_with_launch(env, LaunchParams::default());
}

fn register_calling_agent_with_launch(env: &Env, launch: LaunchParams) {
    let store = env.store();
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace resolves");
    store
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("provider-session"),
                launch_id: Some(AgentSessionId::from("launch-session")),
                agent_name: "planner".to_owned(),
                agent_name_explicit: true,
                launch,
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("planner".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched target");
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("provider-session")),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some("planner".to_owned());
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

fn agent_wait(env: &Env) -> std::process::Command {
    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("RIMZ_AGENT_NAME", "planner");
    command
}
