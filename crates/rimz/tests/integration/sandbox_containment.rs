use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rimz::testkit::sandbox::{SandboxSpec, sandbox_processes};
use serde::Deserialize;

use crate::common::Env;

const CLEANUP_WAIT: Duration = Duration::from_secs(12);

#[derive(Deserialize)]
struct FakeOwnerReport {
    spec: SandboxSpec,
    child_pid: u32,
}

#[test]
fn env_drop_reaps_marker_children_before_removing_roots() {
    let env = Env::new();
    let spec = SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };
    let mut marker_child = env
        .rimz_at(Path::new("sleep"))
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn marker child");
    assert!(
        sandbox_processes(&spec).contains(&marker_child.id()),
        "the fixture roots identify its child before cleanup"
    );

    drop(env);

    wait_for_child_exit(&mut marker_child);
    assert!(!spec.home_root.exists(), "test HOME removed");
    assert!(!spec.runtime_root.exists(), "test runtime removed");
    assert!(
        sandbox_processes(&spec).is_empty(),
        "no process retains the fixture marker"
    );
}

#[test]
fn env_unwind_reaps_marker_children_and_roots() {
    let env = Env::new();
    let spec = SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };
    let mut marker_child = env
        .rimz_at(Path::new("sleep"))
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn marker child");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _env = env;
        panic!("exercise fixture unwind cleanup");
    }));

    assert!(result.is_err());
    wait_for_child_exit(&mut marker_child);
    assert!(!spec.home_root.exists(), "test HOME removed");
    assert!(!spec.runtime_root.exists(), "test runtime removed");
    assert!(sandbox_processes(&spec).is_empty());
}

#[test]
fn owner_sigkill_still_reaps_descendants_and_roots() {
    let mut owner = Command::new(env!("CARGO_BIN_EXE_rimz-test-reaper"))
        .arg("--fake-owner")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fake sandbox owner");
    let report: FakeOwnerReport = {
        let stdout = owner.stdout.take().expect("fake-owner stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read fake-owner report");
        serde_json::from_str(&line).expect("parse fake-owner report")
    };
    assert!(
        sandbox_processes(&report.spec).contains(&report.child_pid),
        "fake owner's child carries its sandbox marker"
    );

    owner.kill().expect("SIGKILL fake owner");
    owner.wait().expect("wait fake owner");

    let deadline = Instant::now() + CLEANUP_WAIT;
    while Instant::now() < deadline
        && (report.spec.home_root.exists()
            || report.spec.runtime_root.exists()
            || Path::new(&format!("/proc/{}", report.child_pid)).exists())
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!report.spec.home_root.exists(), "test HOME removed");
    assert!(!report.spec.runtime_root.exists(), "test runtime removed");
    assert!(
        !Path::new(&format!("/proc/{}", report.child_pid)).exists(),
        "marker child exited after its owner died"
    );
    assert!(
        sandbox_processes(&report.spec).is_empty(),
        "no process retains the dead owner's marker"
    );
}

fn wait_for_child_exit(child: &mut Child) {
    let deadline = Instant::now() + CLEANUP_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => panic!("marker child {} survived cleanup", child.id()),
            Err(err) => panic!("waiting for marker child {}: {err}", child.id()),
        }
    }
}
