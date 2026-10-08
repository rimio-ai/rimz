//! Bounded mux subprocess I/O without a live multiplexer.

#![cfg(unix)]

use std::thread::{self, spawn};
use std::time::{Duration, Instant};

use rimz::mux::{CommandSpec, MuxErr};

fn probe_thread_id() -> u64 {
    let id = spawn(|| thread::current().id())
        .join()
        .expect("probe thread");
    format!("{id:?}")
        .strip_prefix("ThreadId(")
        .and_then(|id| id.strip_suffix(')'))
        .expect("ThreadId debug format")
        .parse()
        .expect("numeric ThreadId")
}

#[cfg(target_os = "linux")]
fn thread_count() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .expect("process status")
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .expect("thread count")
        .trim()
        .parse()
        .expect("numeric thread count")
}

#[test]
fn bounded_runs_create_no_threads() {
    #[cfg(target_os = "linux")]
    let threads = thread_count();
    // Nextest runs each test in its own process, so only our calls can advance
    // std's thread-id counter between the two probe threads.
    let before = probe_thread_id();
    for _ in 0..4 {
        let output = CommandSpec::new("sh")
            .args(["-c", "printf out; printf err >&2"])
            .run()
            .expect("plain command");
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }
    let bytes = vec![b'x'; 256 * 1024];
    let output = CommandSpec::new("cat")
        .stdin_bytes(bytes.clone())
        .run()
        .expect("stdin round trip");
    assert_eq!(output.stdout, bytes);
    for script in ["sleep 2 & exec sleep 2", "sleep 2 & exit 0"] {
        let err = CommandSpec::new("sh")
            .args(["-c", script])
            .run_with_timeout(Duration::from_millis(100))
            .expect_err("descendant holds pipes past the deadline");
        assert!(matches!(err, MuxErr::Timeout { .. }));
    }
    #[cfg(target_os = "linux")]
    assert_eq!(thread_count(), threads, "bounded runs leave no threads");
    assert_eq!(
        probe_thread_id(),
        before + 1,
        "bounded runs spawn no threads"
    );
}

#[test]
fn command_stderr_wins_over_an_unwritten_payload_on_failure() {
    let err = CommandSpec::new("sh")
        .args(["-c", "echo boom >&2; exit 3"])
        .stdin_bytes(vec![b'x'; 1024 * 1024])
        .run()
        .expect_err("command failed without consuming input");
    assert!(matches!(err, MuxErr::Command { stderr, .. } if stderr.contains("boom")));
}

#[test]
fn command_stdin_round_trips_binary_payload_and_redacts_debug() {
    let bytes = b"private paste\0\r\n\xff".repeat(16 * 1024);
    let spec = CommandSpec::new("cat").stdin_bytes(bytes.clone());
    assert!(!format!("{spec:?}").contains("private paste"));
    assert!(!spec.display_line().contains("private paste"));
    let output = spec.run().expect("cat reads through EOF");
    assert_eq!(output.stdout, bytes);
}

#[test]
fn command_stdin_timeout_bounds_a_blocked_writer() {
    let started = Instant::now();
    let err = CommandSpec::new("sleep")
        .arg("30")
        .stdin_bytes(vec![b'x'; 1024 * 1024])
        .run_with_timeout(Duration::from_millis(100))
        .expect_err("non-reading child times out");
    assert!(matches!(err, MuxErr::Timeout { .. }));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn command_stdin_reports_incomplete_input_after_successful_exit() {
    let err = CommandSpec::new("true")
        .stdin_bytes(vec![b'x'; 1024 * 1024])
        .run()
        .expect_err("successful exit did not consume the payload");
    assert!(matches!(err, MuxErr::Io(_)));
}

#[test]
fn command_timeout_bounds_pipes_inherited_after_child_exit() {
    let err = CommandSpec::new("sh")
        .args(["-c", "sleep 1 & exit 0"])
        .stdin_bytes(vec![b'x'; 1024 * 1024])
        .run_with_timeout(Duration::from_millis(100))
        .expect_err("descendant holds output pipes past the deadline");
    assert!(matches!(err, MuxErr::Timeout { .. }));
}
