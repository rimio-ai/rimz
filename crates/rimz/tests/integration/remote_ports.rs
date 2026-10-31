use super::*;
use rimz::disk::lock::WorkspaceLock;
use serde_json::{Value, json};

fn record(pid: u32, started_at_ms: u64) -> Value {
    json!({
        "target":"dev", "host":"dev-box", "pid":pid,
        "started_at_ms":started_at_ms, "at_ms":1_000_000, "auto_forward":true,
        "forwards":[{"remote":3000,"local":3001,"state":"forwarded","since_ms":940_000}]
    })
}

fn records_dir(env: &Env) -> PathBuf {
    env.runtime_root.join("rimz/link/forwards")
}

fn seed_record(env: &Env, pid: u32, value: &Value) -> PathBuf {
    let dir = records_dir(env);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{pid}.json"));
    std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

#[test]
fn machine_scope_shows_only_live_locks_oldest_first_and_reaps_dead_records() {
    let env = Env::new();
    let mut older_record = record(111, 10);
    older_record["forwards"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "remote":4000,"state":"paused","since_ms":940_000
        }));
    let older = seed_record(&env, 111, &older_record);
    let newer = seed_record(&env, 222, &record(222, 20));
    let stale = seed_record(&env, 333, &record(333, 30));
    let orphan = seed_record(&env, 444, &record(444, 40));
    let older_lock = WorkspaceLock::try_acquire(&older.with_extension("lock"))
        .unwrap()
        .unwrap();
    let _newer_lock = WorkspaceLock::try_acquire(&newer.with_extension("lock"))
        .unwrap()
        .unwrap();
    drop(WorkspaceLock::try_acquire(&stale.with_extension("lock")).unwrap());
    let out = env
        .rimz()
        .args(["remote", "ports"])
        .bounded_output()
        .unwrap();
    let text = stdout_line(&out);
    assert!(text.starts_with("2 connections on this machine"), "{text}");
    assert!(
        text.contains("connection to dev-box") && text.contains("remapped"),
        "{text}"
    );
    assert!(
        text.contains("unknown") && text.contains("state not understood"),
        "{text}"
    );
    let out = env
        .rimz()
        .args(["remote", "ports", "--json"])
        .bounded_output()
        .unwrap();
    let value: Value = serde_json::from_str(&stdout_line(&out)).unwrap();
    assert_eq!(value["scope"], "machine");
    assert_eq!(value["connections"][0]["pid"], 111);
    assert_eq!(value["connections"][1]["pid"], 222);
    assert_eq!(value["connections"][0]["forwards"][1]["state"], "unknown");
    assert!(
        value["connections"][0]["forwards"][0]
            .get("since_ms")
            .is_none()
    );
    assert!(
        value["connections"][0]["forwards"][0]["age_secs"]
            .as_u64()
            .unwrap()
            > 60
    );
    assert!(!stale.exists() && !stale.with_extension("lock").exists());
    assert!(!orphan.exists() && !orphan.with_extension("lock").exists());
    drop(older_lock);
    let out = env
        .rimz()
        .args(["remote", "ports", "--json"])
        .bounded_output()
        .unwrap();
    let value: Value = serde_json::from_str(&stdout_line(&out)).unwrap();
    assert_eq!(value["connections"].as_array().unwrap().len(), 1);
}

#[test]
fn fresh_room_sidecar_selects_connection_scope_with_cross_clock_ages() {
    let env = Env::new();
    env.record(&env.project_root);
    let sidecar = rimz::remote::link::stats_path(&env.runtime_paths());
    let now = rimz::utils::time::unix_now_ms();
    let mut forwards = record(111, 10);
    forwards["forwards"].as_array_mut().unwrap().push(json!({
        "remote":4000,"state":"paused","since_ms":940_000
    }));
    rimz::disk::atomic::write_temp_then_rename_cache(
        &sidecar,
        &json!({
            "v":"rimz.link.v1", "received_at_ms":now - 5000, "client":"client",
            "stats":{"miss_pct":0,"window":1}, "forwards":forwards
        }),
    )
    .unwrap();
    let out = env
        .rimz()
        .args(["remote", "ports"])
        .bounded_output()
        .unwrap();
    let text = stdout_line(&out);
    assert!(text.starts_with("connection to dev-box"), "{text}");
    assert!(
        text.contains("remapped") && text.contains("3000 is busy here"),
        "{text}"
    );
    assert!(
        text.contains("unknown") && text.contains("state not understood"),
        "{text}"
    );
    let out = env
        .rimz()
        .args(["remote", "ports", "--json"])
        .bounded_output()
        .unwrap();
    let value: Value = serde_json::from_str(&stdout_line(&out)).unwrap();
    assert_eq!(value["scope"], "connection");
    assert_eq!(value["connections"][0]["forwards"][1]["state"], "unknown");
    let age = value["connections"][0]["forwards"][0]["age_secs"]
        .as_u64()
        .unwrap();
    assert!(
        (65..100).contains(&age),
        "age uses deltas, not the host minus laptop clock: {age}"
    );
}

#[test]
fn legacy_room_sidecar_says_no_forward_data() {
    let env = Env::new();
    env.record(&env.project_root);
    let sidecar = rimz::remote::link::stats_path(&env.runtime_paths());
    let file = rimz::remote::link::LinkStatsFile::new(
        rimz::utils::time::unix_now_ms(),
        "old laptop".into(),
        Default::default(),
    );
    rimz::disk::atomic::write_temp_then_rename_cache(&sidecar, &file).unwrap();
    let out = env
        .rimz()
        .args(["remote", "ports"])
        .bounded_output()
        .unwrap();
    assert!(stdout_line(&out).contains("no forward data from this connection"));
    let out = env
        .rimz()
        .args(["remote", "ports", "--json"])
        .bounded_output()
        .unwrap();
    let value: Value = serde_json::from_str(&stdout_line(&out)).unwrap();
    assert_eq!(value, json!({"scope":"connection","connections":[]}));
}

#[test]
fn absent_expired_or_wrong_version_sidecars_fall_back_to_machine_scope() {
    let env = Env::new();
    env.record(&env.project_root);
    let sidecar = rimz::remote::link::stats_path(&env.runtime_paths());
    for (version, received_at_ms) in [
        (None, 0),
        (Some("rimz.link.v1"), 0),
        (Some("rimz.link.v2"), rimz::utils::time::unix_now_ms()),
    ] {
        if let Some(version) = version {
            rimz::disk::atomic::write_temp_then_rename_cache(
                &sidecar,
                &json!({
                    "v":version,"received_at_ms":received_at_ms,"client":"client",
                    "stats":{"miss_pct":0,"window":1},"forwards":record(111, 10)
                }),
            )
            .unwrap();
        }
        let out = env
            .rimz()
            .args(["remote", "ports"])
            .bounded_output()
            .unwrap();
        let text = stdout_line(&out);
        assert!(text.starts_with("0 connections on this machine"), "{text}");
        assert!(
            text.contains("no rimz remote connect is running on this machine"),
            "{text}"
        );
    }
}

#[test]
fn unavailable_machine_record_does_not_stop_connect_or_forwarding() {
    let env = Env::new();
    let dir = records_dir(&env);
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::fs::write(&dir, b"not a directory").unwrap();
    let log = env.project_root.join("ssh-trace.log");
    let port = crate::common::available_loopback_port();
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_REMOTE_PROBE_MS", "10")
        .env("RIMZ_TEST_PROBE_PORT", port.to_string())
        .env("RIMZ_TEST_PROBE_PORTS_SCOPE", "room")
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "500")
        .env("RIMZ_TEST_SSH_SLEEP_MS", "500")
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "connect must survive: {stderr}");
    let invocations = shim_invocations(&log);
    assert_eq!(
        invocations
            .iter()
            .filter(|argv| argv.windows(2).any(|args| args == ["-O", "forward"]))
            .count(),
        1,
        "forwarding must survive: {invocations:?}"
    );
    assert_eq!(
        stderr
            .matches("publishing remote forward record failed")
            .count(),
        1,
        "publication warns only once: {stderr}"
    );
    let out = env
        .rimz()
        .args(["remote", "ports"])
        .bounded_output()
        .unwrap();
    assert!(stdout_line(&out).starts_with("0 connections on this machine"));
}

#[test]
fn supervisor_publishes_while_alive_and_removes_its_record_on_return() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let ready = env.project_root.join("control-master-ready");
    let ready_plan = env.project_root.join("ssh-master-ready.plan");
    std::fs::write(&ready_plan, "0\n").unwrap();
    let port = crate::common::available_loopback_port();
    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_TEST_SSH_MASTER_READY_PLAN", &ready_plan)
        .env("RIMZ_REMOTE_PROBE_MS", "10")
        .env("RIMZ_TEST_PROBE_PORT", port.to_string())
        .env("RIMZ_TEST_PROBE_PORTS_SCOPE", "room")
        .env("RIMZ_TEST_SSH_SLEEP_MS", "3000")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let path = records_dir(&env).join(format!("{pid}.json"));
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while master_invocation_count(&log) == 0 {
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "first master never started: {:?}",
                std::fs::read_to_string(&log)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = env
        .rimz()
        .args(["remote", "ports", "--json"])
        .bounded_output()
        .unwrap();
    let waiting: Value = serde_json::from_str(&stdout_line(&out)).unwrap();
    let locked = path.with_extension("lock").exists();
    assert_eq!(
        main_invocation_count(&log),
        0,
        "the first master must still be pending: {:?}",
        shim_invocations(&log)
    );
    std::fs::write(&ready, "ready").unwrap();
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let published = loop {
        let value = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if let Some(value) = value.filter(|value| value["forwards"][0]["state"] == "forwarded") {
            break Some(value);
        }
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if published.is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let published = published.expect("a live supervisor publishes its successful forward");
    assert_eq!(waiting["connections"][0]["pid"], pid, "{waiting}");
    assert_eq!(
        waiting["connections"][0]["forwards"],
        json!([]),
        "{waiting}"
    );
    assert!(
        locked,
        "the pending connection holds its record lock: {waiting}"
    );
    assert_eq!(
        published["started_at_ms"],
        waiting["connections"][0]["started_at_ms"]
    );
    assert_eq!(published["pid"], pid);
    assert_eq!(published["forwards"][0]["remote"], port);
    assert!(!path.exists() && !path.with_extension("lock").exists());
}
