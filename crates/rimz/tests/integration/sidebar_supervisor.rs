//! `rimz sidebar serve` supervisor crash capture.

#![cfg(unix)]

use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use rimz::diag::record::{DiagEnvelope, DiagEvent};

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn sidebar_supervisor_records_worker_abort() {
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
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "abort");

    let output = cmd
        .bounded_output_within(Duration::from_secs(10))
        .expect("sidebar serve returns");

    assert!(
        !output.status.success(),
        "worker abort should make supervisor exit non-zero"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rimz test sidebar worker abort"),
        "supervisor should tee worker stderr"
    );

    let diag_path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        None,
    )
    .log_path()
    .unwrap();
    let text = std::fs::read_to_string(&diag_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", diag_path.display()));
    let record: DiagEnvelope = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("diag record"))
        .find(|record: &DiagEnvelope| matches!(record.event, DiagEvent::RendererSignalDeath { .. }))
        .expect("renderer signal death diag");

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
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "sleep_then_exit")
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

    let status = wait_child(&mut child, Duration::from_secs(8));
    assert!(status.success(), "supervisor exited with {status}");
}

#[cfg(target_os = "linux")]
fn read_pid_file(path: &Path, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            return raw.trim().parse().expect("stray pid file contains a pid");
        }
        assert!(Instant::now() < deadline, "stray pid file was not written");
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
