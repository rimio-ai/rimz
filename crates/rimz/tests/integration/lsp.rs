//! Shared language-server configuration and machine registry boundaries.

use crate::common::Env;
use assert_cmd::assert::OutputAssertExt;
use rimz::config::{MachineConfig, effective};

#[test]
fn lsp_required_launch_waits_then_refuses_before_recording_a_run() {
    let env = Env::new();
    crate::common::write_kind_base(&env, "claude");
    env.install_agent_hooks("claude");
    let agent_bin = crate::common::write_failing_agent_shim(&env, "claude", 1);
    let shell = crate::common::write_fake_login_shell(&env, "lsp-test-sh", &[]);
    std::fs::write(env.rimz_home().join("theme.toml"), "[broken").unwrap();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), "[agents]\nisolation = 'host'\n[lsp]\nreserve-min = '1000000G'\n[lsp.servers.rust]\ncommand = ['/bin/true']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\npolicy = 'required'\nwait-timeout = '1s'\n").unwrap();
    let output = env
        .rimz()
        .args(["agents", "claude", "inspect", "-p"])
        .env("SHELL", shell)
        .env("PATH", crate::common::path_with_front(&agent_bin))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("waiting to start language server rust"),
        "{stderr}"
    );
    assert!(stderr.contains("position 1 in the queue"), "{stderr}");
    assert!(
        stderr.contains("required but memory stayed short for 1s"),
        "{stderr}"
    );
    assert!(stderr.contains("stop one with rimz lsp stop"), "{stderr}");
    assert!(
        rimz::harness::run::list(env.store().paths())
            .unwrap()
            .is_empty()
    );
    let output = env.rimz().args(["doctor", "--json"]).output().unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["lsp"]["ready"]["last_refusal"]["event"],
        "queue_timeout"
    );
}

#[test]
fn lsp_broker_starts_lazily_watches_saves_and_restarts() {
    use rimz::lsp::{admission::ServeRequest, registry};
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let env = Env::new();
    let stub = crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"));
    let config: rimz::config::LspServerConfig = serde_json::from_value(
        json!({"command": [stub], "extensions": ["rs"], "root-markers": ["Cargo.toml"], "memory-estimate": "1M"}),
    )
    .unwrap();
    let mut machine = MachineConfig::default();
    machine.lsp.servers.insert("rust".into(), config.clone());
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        toml::to_string(&std::collections::BTreeMap::from([("lsp", &machine.lsp)])).unwrap(),
    )
    .unwrap();
    let request = ServeRequest {
        root: env.project_root.canonicalize().unwrap(),
        project: env.project_root.join("parent-project"),
        server: "rust".into(),
        settings_hash: rimz::lsp::history::settings_hash(&config),
        config,
        policy: rimz::config::LspConfig {
            kill_floor_percent: 0,
            reserve_percent: 0,
            reserve_min: "0".into(),
            ..Default::default()
        },
        eager: false,
    };
    let directory = env
        .runtime_root
        .join("rimz/lsp")
        .join(registry::key(&request.root, "rust").unwrap());
    let mut broker = env
        .rimz()
        .args([
            "lsp",
            "serve",
            "--request",
            &serde_json::to_string(&request).unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !directory.join("entry.json").exists() {
        assert!(
            broker.try_wait().unwrap().is_none(),
            "broker failed before publishing"
        );
        assert!(Instant::now() < deadline, "broker startup deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
    let rpc = |value: Value| -> Value {
        let mut stream = UnixStream::connect(directory.join("sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        writeln!(stream, "{value}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    };
    let pid = std::process::id();
    let status = rpc(json!({"op": "status"}));
    assert!(status["state"].get("dormant").is_some(), "{status}");
    assert!(status["server_pid"].is_null());
    let lease = json!({"op": "lease", "pid": pid, "start_token": rimz::proc::process_start_token(pid).unwrap()});
    assert_eq!(rpc(lease.clone())["ok"], true);
    assert_eq!(rpc(lease)["ok"], true);
    assert_eq!(
        rpc(json!({"op": "status"}))["leases"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let query = |name| {
        rpc(
            json!({"op": "query", "method": "workspace/symbol", "params": {"query": name}, "wait_ms": 4000}),
        )
    };
    assert_eq!(query("symbol")["result"], json!([]));
    let status = rpc(json!({"op": "status"}));
    assert_eq!(status["state"], "ready");
    assert_eq!(status["request_count"], 1);
    let server_pid = status["server_pid"].as_u64().unwrap();
    assert_eq!(
        std::fs::read_to_string(format!("/proc/{server_pid}/oom_score_adj"))
            .unwrap()
            .trim(),
        "800"
    );
    std::fs::write(env.project_root.join("lib.rs"), "fn saved() {}\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = query("changes");
        if response["result"]["changes"]
            .as_array()
            .is_some_and(|changes| {
                changes
                    .iter()
                    .any(|change| change["uri"].as_str().unwrap().ends_with("/lib.rs"))
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "save never reached server: {response}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    env.rimz()
        .args(["lsp", "hover", "lib.rs:1:4"])
        .assert()
        .success()
        .stdout("fixture hover\n");
    env.rimz()
        .args(["lsp", "find", "anything", "--json"])
        .assert()
        .success()
        .stdout("[]\n");
    for verb in ["def", "refs", "impl", "callers", "callees", "symbols"] {
        let target = if verb == "symbols" {
            "lib.rs"
        } else {
            "lib.rs:1:4"
        };
        env.rimz()
            .args(["lsp", verb, target, "--json"])
            .assert()
            .success()
            .stdout("[]\n");
    }
    env.rimz()
        .args(["lsp", "def", "alias"])
        .assert()
        .success()
        .stdout("/fixture/definition.rs:1:1\n");
    for name in [
        "deep::pathed",
        "crate::deep::pathed",
        "deep::pathed::pathed",
        "pathed()",
        "a::twin",
    ] {
        env.rimz()
            .args(["lsp", "def", name])
            .assert()
            .success()
            .stderr("")
            .stdout(if name == "a::twin" {
                "src/a.rs:1:1\n"
            } else {
                "src/deep/pathed.rs:1:1\n"
            });
    }
    env.rimz().args(["lsp", "def", "wrong::pathed"]).assert().code(5).stderr("").stdout("not found: wrong::pathed; 1 symbol named pathed:\nfunction deep::pathed::pathed  src/deep/pathed.rs:1:1\n");
    env.rimz()
        .args(["lsp", "def", "nosuch"])
        .assert()
        .code(5)
        .stderr("")
        .stdout("not found: nosuch\n");
    env.rimz().args(["lsp", "def", "twin"]).assert().code(6).stderr("").stdout("ambiguous: 2 symbols named twin; rerun with one of these names or a position\nfunction a::twin  src/a.rs:1:1\nfunction b::twin  src/b.rs:1:1\n");
    for (name, code, outcome, candidates) in [
        ("nosuch", 5, "not-found", json!([])),
        (
            "twin",
            6,
            "ambiguous",
            json!([
                {"name": "a::twin", "kind": "function", "position": "src/a.rs:1:1"},
                {"name": "b::twin", "kind": "function", "position": "src/b.rs:1:1"}
            ]),
        ),
    ] {
        let output = env
            .rimz()
            .args(["lsp", "def", name, "--json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            json!({"outcome": outcome, "name": name, "candidates": candidates})
        );
    }
    env.rimz().args(["lsp", "find", "work"]).assert().success().stdout("function w::work  src/w.rs:1:1\nfunction w::worker  src/w.rs:1:1\nfunction r::rework  src/r.rs:1:1\nfunction u::unrelated  src/u.rs:1:1\n");
    env.rimz()
        .args(["lsp", "callees", "lib.rs:1:4", "--external", "--json"])
        .assert()
        .success()
        .stdout("[]\n");
    for verb in ["def", "refs", "hover", "impl", "symbols", "find"] {
        env.rimz()
            .args(["lsp", verb, "lib.rs:1:4", "--external"])
            .assert()
            .code(2);
    }
    let mut owner = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let owner_pid = owner.id();
    assert_eq!(
        rpc(
            json!({"op": "lease", "launch_id": "reaped", "pid": owner_pid, "start_token": rimz::proc::process_start_token(owner_pid).unwrap()})
        )["ok"],
        true
    );
    owner.kill().unwrap();
    owner.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(7);
    while rpc(json!({"op": "status"}))["leases"]
        .as_array()
        .unwrap()
        .len()
        != 1
    {
        assert!(Instant::now() < deadline, "dead lease was not reaped");
        std::thread::sleep(Duration::from_millis(25));
    }
    env.rimz().args(["lsp", "stop"]).assert().success();
    let wait_dormant = |reason: &str, timeout: Duration| {
        let deadline = Instant::now() + timeout;
        loop {
            let status = rpc(json!({"op": "status"}));
            if status["state"]["dormant"]["reason"] == reason && status["server_pid"].is_null() {
                return status;
            }
            assert!(Instant::now() < deadline, "not dormant: {status}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait_dormant("stopped by hand", Duration::from_secs(3));
    env.rimz()
        .args(["lsp", "find", "anything", "--json"])
        .assert()
        .success()
        .stdout("[]\n");
    let status = rpc(json!({"op": "status"}));
    assert_eq!(status["restarts"], 1);
    assert_ne!(status["server_pid"], server_pid);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(status["server_pid"].as_i64().unwrap() as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    wait_dormant("crashed", Duration::from_secs(1));
    assert_eq!(query("ready")["result"], json!([]));
    assert_eq!(rpc(json!({"op": "status"}))["restarts"], 2);
    assert_eq!(
        rpc(json!({"op": "stop", "reason": "checkout removed"}))["ok"],
        true
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while broker.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "terminal stop did not exit with a live lease"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!directory.exists());

    let history = std::fs::read_to_string(env.rimz_home().join("lsp-history.jsonl")).unwrap();
    let record: Value = serde_json::from_str(history.lines().last().unwrap()).unwrap();
    assert_eq!(record["project"], json!(request.project));
    assert_eq!(record["root"], json!(request.root));
    let records: Vec<Value> = history
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["reason"], "stopped by hand");
    assert_eq!(records[1]["reason"], "crashed");
    assert!(records[0]["dormant_ms"].is_null());
    assert!(records[1]["dormant_ms"].is_u64());
    assert!(records[2]["dormant_ms"].is_u64());

    let mut request = request;
    request.policy.idle_timeout = "2s".into();
    let (mut broker, _) = spawn_test_broker(&env, &request);
    assert_eq!(query("ready")["result"], json!([]));
    assert_eq!(query("slow")["result"], json!([]));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = rpc(json!({"op": "status"}));
        if status["state"]["dormant"]["reason"] == "idle" && status["server_pid"].is_null() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "idle server stayed running: {status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(query("ready")["result"], json!([]));
    assert_eq!(rpc(json!({"op": "status"}))["restarts"], 1);
    assert_eq!(
        rpc(json!({"op": "stop", "reason": "checkout removed"}))["ok"],
        true
    );
    broker.wait().unwrap();
    request.policy.reserve_min = "1000000G".into();
    let (mut broker, _) = spawn_test_broker(&env, &request);
    env.rimz()
        .args(["lsp", "find", "anything"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("not started: memory short"));
    let status = rpc(json!({"op": "status"}));
    assert!(status["state"].get("dormant").is_some());
    assert!(status["server_pid"].is_null());
    let output = env.rimz().args(["doctor", "--json"]).output().unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["lsp"]["ready"]["last_refusal"]["event"], "refused");
    assert!(report["lsp"]["ready"]["last_refusal"]["details"]["estimate_bytes"].is_u64());
    assert_eq!(
        rpc(json!({"op": "stop", "reason": "checkout removed"}))["ok"],
        true
    );
    broker.wait().unwrap();
}

#[test]
fn lsp_project_servers_are_inert_until_trusted_and_overlay_whole_entries() {
    let env = Env::new();
    env.write_config(&env.project_root, "[lsp.servers.rust]\ncommand = ['project-ra']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']");
    let machine: MachineConfig = toml::from_str("[lsp.servers.rust]\ncommand = ['machine-ra']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\ninit-options = { checkOnSave = false }").unwrap();
    let config = effective::load_with_roots(&machine, &env.project_root, &env.rimz_home()).unwrap();
    assert_eq!(config.untrusted_lsp_servers, ["rust"]);
    assert_eq!(config.lsp_servers["rust"].command, ["machine-ra"]);
    env.rimz().args(["trust", "grant"]).assert().success();
    let config = effective::load_with_roots(&machine, &env.project_root, &env.rimz_home()).unwrap();
    assert!(config.untrusted_lsp_servers.is_empty());
    assert_eq!(config.lsp_servers["rust"].command, ["project-ra"]);
    assert!(config.lsp_servers["rust"].init_options.is_none());
}

pub(super) fn spawn_test_broker(
    env: &Env,
    request: &rimz::lsp::admission::ServeRequest,
) -> (std::process::Child, std::path::PathBuf) {
    use std::time::{Duration, Instant};
    let directory = env
        .runtime_root
        .join("rimz/lsp")
        .join(rimz::lsp::registry::key(&request.root, &request.server).unwrap());
    let mut broker = env
        .rimz()
        .args([
            "lsp",
            "serve",
            "--request",
            &serde_json::to_string(request).unwrap(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.join("entry.json").exists() {
        assert!(broker.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    (broker, directory)
}

#[test]
fn lsp_machine_policy_in_project_is_refused_even_before_trust() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[lsp]\nreserve-percent = 20",
    )
    .unwrap();
    let error = effective::load_with_roots(&MachineConfig::default(), project.path(), home.path())
        .err()
        .expect("reject project policy");
    assert!(
        error.to_string().contains(
            "project config cannot set lsp.reserve-percent; move it to ~/.rimz/config.toml"
        )
    );
}

#[test]
fn lsp_sweep_removes_reused_pid_but_keeps_live_broker() {
    use rimz::lsp::registry::{self, Entry, State};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let runtime = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let entry = Entry {
        root: runtime.path().join("checkout"),
        project: Some(runtime.path().join("project")),
        server: "live".into(),
        nonce: "nonce".into(),
        broker_pid: pid,
        broker_start_token: rimz::proc::process_start_token(pid).unwrap(),
        server_pid: None,
        server_start_token: None,
        state: State::Dormant {
            reason: Some(rimz::lsp::registry::StopReason::MemoryPressure),
            since_ms: 100,
        },
        started_at_ms: 0,
        ready_at_ms: None,
        estimate_bytes: 8000,
        settings_hash: "hash".into(),
        request_count: 0,
        last_request_at_ms: None,
        peak_rss_kb: 5,
        restarts: 0,
        leases: Vec::new(),
    };
    let live_dir = runtime
        .path()
        .join(registry::key(&entry.root, &entry.server).unwrap());
    rimz::disk::atomic::write_temp_then_rename_cache(&live_dir.join("entry.json"), &entry).unwrap();
    let mut dead = entry.clone();
    dead.server = "dead".into();
    dead.broker_start_token = "not-the-current-process".into();
    let dead_dir = runtime
        .path()
        .join(registry::key(&dead.root, &dead.server).unwrap());
    rimz::disk::atomic::write_temp_then_rename_cache(&dead_dir.join("entry.json"), &dead).unwrap();
    let listener = UnixListener::bind(live_dir.join("sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let started = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && started.elapsed() < Duration::from_secs(5) =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("fake broker accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap()["op"],
            "hello"
        );
        stream.write_all(b"{\"nonce\":\"nonce\"}\n").unwrap();
    });
    let entries = registry::testkit::sweep(runtime.path()).unwrap();
    server.join().unwrap();
    assert_eq!(entries, std::slice::from_ref(&entry));
    assert!(live_dir.exists());
    assert!(!dead_dir.exists());
    assert_eq!(
        registry::testkit::sweep(runtime.path()).unwrap(),
        [entry],
        "an unavailable socket must not orphan a live broker"
    );
}

#[test]
fn lsp_required_queue_orders_waiters_and_reaps_dead_owners() {
    let directory = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let record = serde_json::json!({"need_bytes": 8000, "pid": pid, "start_token": rimz::proc::process_start_token(pid).unwrap()});
    let newer = directory
        .path()
        .join("00000000000000000020-00000000-0000-0000-0000-000000000020");
    let older = directory
        .path()
        .join("00000000000000000010-00000000-0000-0000-0000-000000000010");
    let dead = directory
        .path()
        .join("00000000000000000001-00000000-0000-0000-0000-000000000001");
    std::fs::write(
        directory
            .path()
            .join("00000000000000000001-00000000-0000-0000-0000-000000000001.tmp.1.nonce"),
        "partial",
    )
    .unwrap();
    for path in [&newer, &older] {
        rimz::disk::atomic::write_temp_then_rename_cache(path, &record).unwrap();
    }
    let mut dead_record = record.clone();
    dead_record["start_token"] = serde_json::json!("not-this-process");
    rimz::disk::atomic::write_temp_then_rename_cache(&dead, &dead_record).unwrap();
    assert_eq!(
        rimz::lsp::admission::testkit::queue_order(directory.path()).unwrap(),
        [older.clone(), newer.clone()]
    );
    assert!(!dead.exists());
    std::fs::remove_file(older).unwrap();
    assert_eq!(
        rimz::lsp::admission::testkit::queue_order(directory.path()).unwrap(),
        [newer]
    );
}

#[test]
fn lsp_admission_refuses_untrusted_or_missing_program_before_spawn() {
    use rimz::lsp::admission::{AdmissionRequest, WaitQueue, admit_launch};

    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    let machine: MachineConfig = toml::from_str("[lsp.servers.rust]\ncommand = ['/no-such-language-server/rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']").unwrap();
    let runtime = env.runtime_paths();
    let request = AdmissionRequest {
        root: &env.project_root,
        project: &env.project_root,
        servers: &machine.lsp.servers,
        untrusted_servers: &["rust".to_owned()],
        policy: &machine.lsp,
        runtime: &runtime,
    };
    let error = admit_launch(&request, &mut WaitQueue::default())
        .err()
        .expect("untrusted project must refuse");
    assert!(error.to_string().contains("run rimz trust"));
    let request = AdmissionRequest {
        untrusted_servers: &[],
        ..request
    };
    let error = admit_launch(&request, &mut WaitQueue::default())
        .err()
        .expect("missing server must refuse");
    assert!(
        error
            .to_string()
            .contains("install it or remove [lsp.servers.rust]")
    );
}

#[test]
fn lsp_required_launch_has_one_queue_slot_for_all_its_servers() {
    use rimz::lsp::admission::testkit::{enqueue_servers, queue_order};

    let directory = tempfile::tempdir().unwrap();
    let older = enqueue_servers(directory.path(), &[8000, 4000]).unwrap();
    let older_paths = queue_order(directory.path()).unwrap();
    assert_eq!(
        older_paths.len(),
        1,
        "one queue file per launch, not per server"
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&older_paths[0]).unwrap()).unwrap();
    assert_eq!(record["need_bytes"], 12000);
    let newer = enqueue_servers(directory.path(), &[2000]).unwrap();
    let paths = queue_order(directory.path()).unwrap();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], older_paths[0]);
    drop(older);
    assert_eq!(queue_order(directory.path()).unwrap(), [paths[1].clone()]);
    drop(newer);
    assert!(queue_order(directory.path()).unwrap().is_empty());
}
