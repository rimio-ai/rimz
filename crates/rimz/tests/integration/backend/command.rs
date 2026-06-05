//! The shared subprocess engine behind every mux control command
//! (`rimz::mux::CommandSpec`). Subprocess behavior is integration-tier, so the
//! engine's spawn/exit/deadline contract is proven here, against real
//! processes (`true`/`false`/`sleep` — coreutils, present everywhere the
//! suite runs).

use std::time::Duration;

use rimz::mux::{CommandSpec, MuxErr};

#[test]
fn run_returns_quickly_for_a_fast_command() {
    let output = CommandSpec::new("true").run().expect("`true` succeeds");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn run_with_timeout_kills_a_hung_child() {
    let started = std::time::Instant::now();
    let err = CommandSpec::new("sleep")
        .arg("30")
        .run_with_timeout(Duration::from_millis(100))
        .expect_err("the deadline fires");
    assert!(matches!(err, MuxErr::Timeout { .. }), "got: {err}");
    // The kill lands and the waiter reaps promptly — well under the child's
    // own 30s sleep. Loose bound so a loaded CI box never flakes.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout path must not wait out the child",
    );
}

#[test]
fn run_surfaces_a_nonzero_exit() {
    let err = CommandSpec::new("false").run().expect_err("`false` fails");
    assert!(matches!(err, MuxErr::Command { .. }), "got: {err}");
}
