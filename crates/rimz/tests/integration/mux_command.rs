//! Bounded mux subprocess I/O without a live multiplexer.

#![cfg(unix)]

use std::time::{Duration, Instant};

use rimz::mux::{CommandSpec, MuxErr};

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
