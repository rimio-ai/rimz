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
