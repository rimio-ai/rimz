use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rimz::testkit::sandbox::{SandboxSpec, TestSandbox, sandbox_processes};
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
    wait_for_marker_child(&spec, marker_child.id());

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
    wait_for_marker_child(&spec, marker_child.id());

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
    let _home = tempfile::Builder::new()
        .prefix("rimz-test-home-")
        .rand_bytes(6)
        .tempdir()
        .expect("fake-owner HOME");
    let _runtime = tempfile::Builder::new()
        .prefix("rr")
        .rand_bytes(6)
        .tempdir_in("/tmp")
        .expect("fake-owner runtime");
    let spec = SandboxSpec {
        home_root: _home
            .path()
            .canonicalize()
            .unwrap_or_else(|_| _home.path().to_path_buf()),
        runtime_root: _runtime.path().to_path_buf(),
    };
    let encoded = serde_json::to_string(&spec).expect("serialize fake-owner spec");
    let mut owner = Command::new(env!("CARGO_BIN_EXE_rimz-test-reaper"))
        .arg("--fake-owner")
        .arg(encoded)
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
    assert_eq!(report.spec, spec);
    wait_for_marker_child(&report.spec, report.child_pid);

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

fn wait_for_marker_child(spec: &SandboxSpec, pid: u32) {
    let started = Instant::now();
    while !sandbox_processes(spec).contains(&pid) {
        let waited = started.elapsed();
        assert!(
            waited < CLEANUP_WAIT,
            "the fixture roots never identified marker child {pid} after {waited:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
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

#[test]
fn env_unwind_with_keep_on_moves_roots_and_reaps_markers() {
    let kept_parent = kept_parent();
    let mut env = Env::new();
    env.keep_failed_under = Some(kept_parent.path().to_path_buf());
    let spec = SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };
    std::fs::write(env.home_root.join("before-panic.txt"), "state").expect("write home state");
    let mut marker_child = env
        .rimz_at(Path::new("sleep"))
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn marker child");
    wait_for_marker_child(&spec, marker_child.id());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _env = env;
        panic!("exercise fixture keep-on-failure");
    }));

    assert!(result.is_err());
    wait_for_child_exit(&mut marker_child);
    assert!(sandbox_processes(&spec).is_empty());
    assert!(!spec.home_root.exists(), "test HOME moved away");
    assert!(!spec.runtime_root.exists(), "test runtime moved away");
    let kept = kept_dirs(kept_parent.path());
    assert_eq!(kept.len(), 1, "one kept directory: {kept:?}");
    assert_eq!(
        std::fs::read_to_string(kept[0].join("home/before-panic.txt")).expect("kept home state"),
        "state"
    );
    assert!(kept[0].join("runtime").is_dir(), "kept runtime");
}

#[test]
fn env_orderly_drop_with_keep_on_removes_roots() {
    let kept_parent = kept_parent();
    let mut env = Env::new();
    env.keep_failed_under = Some(kept_parent.path().to_path_buf());
    let spec = SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };

    drop(env);

    assert!(!spec.home_root.exists(), "test HOME removed");
    assert!(!spec.runtime_root.exists(), "test runtime removed");
    assert!(kept_dirs(kept_parent.path()).is_empty());
}

#[test]
fn sandbox_fallback_keeps_roots_when_the_reaper_fails() {
    let home = tempfile::Builder::new()
        .prefix("rimz-test-home-")
        .rand_bytes(6)
        .tempdir()
        .expect("fallback HOME");
    let runtime = tempfile::Builder::new()
        .prefix("rr")
        .rand_bytes(6)
        .tempdir_in("/tmp")
        .expect("fallback runtime");
    let spec = SandboxSpec {
        home_root: home
            .path()
            .canonicalize()
            .unwrap_or_else(|_| home.path().to_path_buf()),
        runtime_root: runtime.path().to_path_buf(),
    };
    let sandbox = TestSandbox::arm(spec.clone(), Path::new("false")).expect("arm failing reaper");
    let mut command = Command::new("sleep");
    command
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    sandbox.pin_identity(&mut command);
    let mut marker_child = command.spawn().expect("spawn marker child");
    wait_for_marker_child(&spec, marker_child.id());

    sandbox.reap_keeping_roots();

    wait_for_child_exit(&mut marker_child);
    assert!(sandbox_processes(&spec).is_empty());
    assert!(spec.home_root.is_dir(), "fallback kept HOME");
    assert!(spec.runtime_root.is_dir(), "fallback kept runtime");
}

fn kept_parent() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rimz-test-kept-parent-")
        .tempdir_in("/tmp")
        .expect("kept parent")
}

fn kept_dirs(parent: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(parent)
        .expect("read kept parent")
        .map(|entry| entry.expect("kept parent entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rimz-test-kept-"))
        })
        .collect()
}
