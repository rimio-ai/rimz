//! `rimz sidebar serve` supervisor crash capture.

#![cfg(unix)]

#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::process::{Child, ExitStatus};
#[cfg(target_os = "linux")]
use std::thread;
use std::time::Duration;
use std::time::Instant;

use rimz::diag::record::{DiagEnvelope, DiagEvent};

use crate::common::Env;

#[test]
fn sidebar_supervisor_records_worker_abort_and_respawns() {
    let env = Env::new();
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "abort")
    .stdout(Stdio::null())
    .stderr(Stdio::piped());

    let diag_path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        None,
    )
    .log_path()
    .unwrap();
    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    let deadline = Instant::now() + Duration::from_secs(5);
    let record = loop {
        let record = std::fs::read_to_string(&diag_path).ok().and_then(|text| {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| serde_json::from_str::<DiagEnvelope>(line).ok())
                .find(|record| matches!(record.event, DiagEvent::RendererSignalDeath { .. }))
        });
        if let Some(record) = record {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "renderer signal death diag timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "worker abort must leave the pane-resident supervisor running",
    );
    child.kill().expect("stop respawning supervisor");
    let output = child.wait_with_output().expect("collect supervisor output");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rimz test sidebar worker abort"),
        "supervisor should tee worker stderr",
    );

    match record.event {
        DiagEvent::RendererSignalDeath {
            signal,
            exit_code,
            stderr_excerpt,
        } => {
            assert_eq!(signal, Some(6));
            assert_eq!(exit_code, None);
            assert!(stderr_excerpt.contains("rimz test sidebar worker abort"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
#[cfg(target_os = "linux")]
fn sidebar_supervisor_reaps_stray_children_while_worker_runs() {
    let env = Env::new();
    let stray_pid_path = env.home_root.join("stray.pid");
    let worker_exit_path = env.home_root.join("worker.exit");
    std::fs::write(&stray_pid_path, b"").expect("seed empty stray pid file");
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "exit_on_file")
    .env("RIMZ_TEST_SIDEBAR_WORKER_EXIT_FILE", &worker_exit_path)
    .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS", "10")
    .env(
        "RIMZ_TEST_SIDEBAR_SUPERVISOR_STRAY_PID_FILE",
        &stray_pid_path,
    )
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    let stray_pid = read_pid_file(&stray_pid_path, Duration::from_secs(2));

    wait_for_reap(stray_pid, Duration::from_secs(3));
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "worker should still be running when the supervisor reaps the stray child"
    );

    std::fs::write(&worker_exit_path, b"done").expect("release sidebar worker");
    let status = wait_child(&mut child, Duration::from_secs(2));
    assert!(status.success(), "supervisor exited with {status}");
}

#[test]
#[cfg(target_os = "linux")]
fn sidebar_supervisor_reaps_worker_when_its_pane_disappears() {
    let env = Env::new();
    let instance =
        rimz::SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da04").expect("instance id");
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let heartbeat_path = runtime.sidebar_heartbeat_path(&instance);
    let socket_path = runtime
        .sock_dir
        .join(format!("sidebar.{}.sock", instance.short()));
    std::fs::write(&heartbeat_path, b"seed heartbeat").expect("seed heartbeat");
    std::fs::write(&socket_path, b"seed socket").expect("seed socket");

    let worker_exit_path = env.home_root.join("worker-never-exits");
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_SIDEBAR_INSTANCE_ID", instance.as_str())
    .env("TMUX_PANE", "%11")
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "exit_on_file")
    .env("RIMZ_TEST_SIDEBAR_WORKER_EXIT_FILE", &worker_exit_path)
    .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS", "10")
    .env("RIMZ_TEST_SIDEBAR_PANE_PROBE_INTERVAL_MS", "10")
    .env("RIMZ_TEST_SIDEBAR_PANE_PROBE", "absent")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    let status = wait_child(&mut child, Duration::from_secs(3));

    assert!(status.success(), "supervisor exited with {status}");
    assert!(!heartbeat_path.exists(), "orphan heartbeat must be removed");
    assert!(!socket_path.exists(), "orphan socket must be removed");
    let diag_path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        Some(instance),
    )
    .log_path()
    .expect("diag path");
    let record = std::fs::read_to_string(diag_path)
        .expect("orphan reap diagnostic")
        .lines()
        .filter_map(|line| serde_json::from_str::<DiagEnvelope>(line).ok())
        .find(|record| matches!(record.event, DiagEvent::RendererOrphanReaped { .. }))
        .expect("renderer orphan reap event");
    assert!(matches!(
        record.event,
        DiagEvent::RendererOrphanReaped {
            ref pane_id,
            worker_pid,
        } if pane_id == "tmux:%11" && worker_pid > 0
    ));
}

#[cfg(target_os = "linux")]
fn read_pid_file(path: &Path, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    let mut last_raw = None;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(pid) = raw.trim().parse() {
                return pid;
            }
            last_raw = Some(raw);
        }
        assert!(
            Instant::now() < deadline,
            "stray pid file did not contain a pid: {:?}",
            last_raw.as_deref()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn wait_for_reap(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let proc_path = Path::new("/proc").join(pid.to_string());
    loop {
        if !proc_path.exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "stray child {pid} was not reaped"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn wait_child(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll supervisor") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sidebar supervisor did not finish within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
