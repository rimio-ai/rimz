//! Integration coverage for the agent-facing `rimz wake` doorway.

use crate::common::Env;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::config::Tasks;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use rimz::store::writer::AgentLifecycleIntent;

#[test]
fn wake_delay_arms_instance_for_the_calling_agent() {
    let env = Env::new();
    register_calling_agent(&env);
    let stdout = wake_ok(&env, &["wake", "--in", "5m"]);
    assert!(stdout.starts_with("armed wake-"), "{stdout}");
    assert!(stdout.contains("in 5m"), "{stdout}");
    assert!(stdout.contains("→ @planner"), "{stdout}");
    let tasks = wake_instances(&env);
    assert_eq!(tasks.0.len(), 1);
    let (name, entry) = tasks.0.iter().next().unwrap();
    assert!(name.starts_with("wake-"));
    assert!(entry.at.is_some());
    assert!(entry.signal.is_none());
    assert!(entry.deadline.is_none());
    assert!(entry.prompt.is_none());
    assert!(entry.wake_meta.is_some());
    let target = entry.wake.as_ref().expect("pinned wake target");
    assert_eq!(target.kind.as_str(), "claude");
    assert_eq!(target.session.as_str(), "provider-session");
    assert_eq!(target.handle, "@planner#project");
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
            "--wake",
            "@planner",
            "--signal",
            "deploy.failed",
        ])
        .output()
        .expect("arm wake from human shell");
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let name = "deployment";

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
        format!("canceled {name}\nno pending wakes\n")
    );
}

#[test]
fn wake_arm_refuses_a_plain_shell() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["wake", "--in", "5m"])
        .output()
        .expect("run wake from shell");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("arming a wake is only available to an agent"),
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
fn wake_rejects_watch_checkins_at_or_above_24_hours() {
    let env = Env::new();
    register_calling_agent(&env);
    for timeout in ["24h", "25h"] {
        let output = agent_wake(&env)
            .args(["wake", "--timeout", timeout, "--", "true"])
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
fn watched_failure_preserves_full_output_and_delivers_its_tail() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let output = agent_wake(&env)
        .args([
            "wake",
            "--json",
            "--",
            "sh",
            "-c",
            "sleep 1; seq 1 5000; printf watched; exit 3",
        ])
        .output()
        .expect("arm watched wake");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = wait_for_wake_records(&env, 1);
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
    assert_eq!(wait_for_wake_messages(&env, 1).len(), 1);

    wait_for_no_wake_instances(&env);
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
    let message = wait_for_wake_messages(&env, 1).pop().unwrap();
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
    let message = wait_for_wake_messages(&env, 1).pop().unwrap();
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
    for (on, command) in [("fail", "true"), ("success", "false")] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        wake_ok(&env, &["wake", "--on", on, "--", command]);
        let records = wait_for_wake_records(&env, 1);
        assert_eq!(records[0].result.label(), "skipped");
        assert!(records[0].message_id.is_none());
        assert!(env.store().list_pending_messages().unwrap().is_empty());
        wait_for_no_wake_instances(&env);
    }
}

#[test]
fn self_wake_queues_with_any_gate_for_working_and_idle_targets() {
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
        wake_ok(&env, &["wake", "--", "printf", "self-wake-marker"]);
        let messages = wait_for_wake_messages(&env, 1);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent_id.as_str(), "provider-session");
        assert_eq!(messages[0].gate, DeliveryGate::Any);
        assert_eq!(
            messages[0].sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wake
            }
        );
        assert!(messages[0].text.contains("self-wake-marker"));
        wait_for_no_wake_instances(&env);
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
        let receipt = wake_ok(
            &env,
            &[
                "wake",
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
        let messages = wait_for_wake_messages(&env, 1);
        let notice = &messages[0];
        assert!(
            notice.text.contains("still running after"),
            "--on {on}: {}",
            notice.text
        );
        assert!(notice.text.contains("checkin-marker"), "{}", notice.text);
        assert!(
            notice.text.contains(&format!("rimz wake cancel {name}")),
            "{}",
            notice.text
        );
        assert!(notice.text.contains("rimz wake --in 1s"), "{}", notice.text);
        assert_eq!(notice.agent_id.as_str(), "provider-session");
        assert_eq!(
            notice.sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wake
            }
        );
        assert_eq!(notice.gate, DeliveryGate::Any);
        let pid: u32 = std::fs::read_to_string(&pid_path).unwrap().parse().unwrap();
        let observe_until = std::time::Instant::now() + std::time::Duration::from_millis(1200);
        while std::time::Instant::now() < observe_until {
            assert!(
                rimz::proc::process_is_live(pid, None),
                "check-in killed the command"
            );
            assert!(
                wake_instances(&env).0.contains_key(name),
                "check-in consumed the instance"
            );
            assert_eq!(
                env.store().list_pending_messages().unwrap().len(),
                1,
                "duplicate check-in"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let records = wait_for_wake_records(&env, 1);
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
        let records = wait_for_wake_records(&env, 2);
        assert_eq!(records.len(), 2);
        assert!(matches!(
            records[1].watch.as_ref().unwrap(),
            rimz::harness::schedule::signal::WatchVerdict::Exited { code: Some(code), .. }
                if code.to_string() == exit
        ));
        wait_for_no_wake_instances(&env);
        let messages = wait_for_wake_messages(&env, if delivers_exit { 2 } else { 1 });
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
                final_message.text.contains("final-marker"),
                "{}",
                final_message.text
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
fn once_wake_subscriber_is_consumed_by_watcher_checkin() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    wake_ok(
        &env,
        &[
            "loop", "add", "audit", "--signal", "wake.*", "--wake", "@me", "--once",
        ],
    );
    let release = env.home_root.join("release");
    let receipt = wake_ok(
        &env,
        &[
            "wake",
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
    let records = wait_for_wake_records(&env, 2);
    assert_eq!(records.len(), 2);
    let audit = records
        .iter()
        .find(|record| record.task == "audit")
        .unwrap();
    assert!(matches!(
        audit.watch.as_ref().unwrap(),
        rimz::harness::schedule::signal::WatchVerdict::Running { .. }
    ));
    let messages = wait_for_wake_messages(&env, 2);
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
    let tasks = wake_instances(&env);
    assert!(
        tasks.0.contains_key(name),
        "check-in consumed the watched task"
    );
    assert!(
        !tasks.0.contains_key("audit"),
        "check-in did not consume the once subscriber"
    );

    std::fs::write(&release, "").unwrap();
    wait_for_no_wake_instances(&env);
    wait_until("watcher did not finish its exit delivery", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_none()
    });
    let records = wait_for_wake_records(&env, 3);
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
    assert_eq!(wait_for_wake_messages(&env, 3).len(), 3);
}

#[test]
fn wake_cancel_before_watcher_start_prevents_command() {
    let env = Env::new();
    register_calling_agent(&env);
    let receipt = wake_ok(&env, &["wake", "--in", "5m", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let mut tasks = wake_instances(&env);
    let entry = tasks.0.get_mut(name).unwrap();
    entry.at = None;
    entry.watch = Some("touch command-started".to_owned());
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();

    wake_ok(&env, &["wake", "cancel", name]);
    let output = env.rimz().args(["wake", "watch", name]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no wake named"));
    assert!(!env.project_root.join("command-started").exists());
    assert!(wake_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn wake_cancel_all_stops_command_groups_and_prints_pending() {
    let env = Env::new();
    register_calling_agent(&env);
    let mut pids = Vec::new();
    for index in 0..2 {
        let path = env.home_root.join(format!("command-{index}.pids"));
        wake_ok(
            &env,
            &[
                "wake",
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
    let names = wake_instances(&env).0.into_keys().collect::<Vec<_>>();
    let canceled = wake_ok(&env, &["wake", "cancel", "--all"]);
    assert!(canceled.starts_with("canceled "), "{canceled}");
    for name in names {
        assert!(canceled.contains(&name), "{canceled}");
    }
    assert!(canceled.ends_with("no pending wakes\n"), "{canceled}");
    wait_until("cancel left watched descendants alive", || {
        pids.iter()
            .all(|pid| !rimz::proc::process_is_live(*pid, None))
    });
    assert!(wake_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn wake_receipts_and_list_share_pending_rows() {
    let env = Env::new();
    register_calling_agent(&env);
    let first = wake_ok(&env, &["wake", "--in", "5m", "--json"]);
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    let second = wake_ok(&env, &["wake", "--in", "10m", "--json"]);
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    let listed = wake_ok(&env, &["wake", "list", "--json"]);
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(first["pending"].as_array().unwrap().len(), 1);
    assert_eq!(second["pending"].as_array().unwrap().len(), 2);
    assert_eq!(second["pending"], listed);
    wake_ok(&env, &["loop", "disable", first["name"].as_str().unwrap()]);
    wake_ok(
        &env,
        &[
            "loop",
            "pause",
            second["name"].as_str().unwrap(),
            "--for",
            "1h",
        ],
    );
    let held = wake_ok(&env, &["wake", "list", "--json"]);
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
    let canceled = wake_ok(
        &env,
        &["wake", "cancel", first["name"].as_str().unwrap(), "--json"],
    );
    let canceled: serde_json::Value = serde_json::from_str(&canceled).unwrap();
    assert_eq!(canceled["canceled"], serde_json::json!([first["name"]]));
    assert_eq!(canceled["pending"].as_array().unwrap().len(), 1);
    assert_eq!(canceled["pending"][0]["name"], second["name"]);
    let listed = wake_ok(&env, &["wake", "list", "--json"]);
    assert_eq!(
        canceled["pending"],
        serde_json::from_str::<serde_json::Value>(&listed).unwrap()
    );
    let human = wake_ok(&env, &["wake", "--in", "15m"]);
    assert!(human.starts_with("armed wake-"), "{human}");
    assert!(human.contains(second["name"].as_str().unwrap()), "{human}");
}

#[test]
fn wake_rejects_removed_target_prompt_and_signal_flags() {
    let env = Env::new();
    register_calling_agent(&env);
    for args in [
        vec!["wake", "@planner", "--in", "5m"],
        vec!["wake", "--in", "5m", "--prompt", "note"],
        vec!["wake", "--in", "5m", "--prompt-file", "note.txt"],
        vec!["wake", "--signal", "deploy.failed"],
        vec!["wake", "--in", "5m", "--match", "branch=feature"],
        vec!["wake", "--wait=5s", "--", "true"],
    ] {
        let output = agent_wake(&env).args(&args).output().unwrap();
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

fn wait_for_wake_messages(env: &Env, count: usize) -> Vec<MessageRecord> {
    let mut messages = Vec::new();
    wait_until("expected durable wake message at the consumer", || {
        messages = env.store().list_pending_messages().unwrap();
        messages.len() >= count
    });
    messages
}

fn wait_for_no_wake_instances(env: &Env) {
    wait_until("wake instance was not retired", || {
        wake_instances(env).0.is_empty()
    });
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

fn loop_instances_path(env: &Env) -> std::path::PathBuf {
    env.state_path_for(&env.project_root)
        .root
        .join("loop-instances.json")
}

fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.state_root().join("rimz").join("loop-runs.log.jsonl")
}

fn wake_instances(env: &Env) -> Tasks {
    let path = loop_instances_path(env);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
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

fn register_calling_agent(env: &Env) {
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
                launch: LaunchParams::default(),
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

fn agent_wake(env: &Env) -> std::process::Command {
    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("RIMZ_AGENT_NAME", "planner");
    command
}
