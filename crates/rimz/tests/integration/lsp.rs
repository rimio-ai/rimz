//! Shared language-server configuration and machine registry boundaries.

use crate::common::Env;
use assert_cmd::assert::OutputAssertExt;
use rimz::config::{MachineConfig, effective};

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
fn lsp_sweep_removes_reused_pid_but_keeps_live_tombstone() {
    use rimz::lsp::registry::{self, Entry, State};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let runtime = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let entry = Entry {
        root: runtime.path().join("checkout"),
        server: "live".into(),
        nonce: "nonce".into(),
        broker_pid: pid,
        broker_start_token: rimz::proc::process_start_token(pid).unwrap(),
        server_pid: None,
        server_start_token: None,
        state: State::Stopped {
            reason: "memory pressure".into(),
            at_ms: 100,
        },
        started_at_ms: 0,
        ready_at_ms: None,
        estimate_bytes: 8000,
        settings_hash: "hash".into(),
        request_count: 0,
        last_request_at_ms: None,
        peak_rss_kb: 5,
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
    assert_eq!(entries, [entry]);
    assert!(live_dir.exists());
    assert!(!dead_dir.exists());
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
