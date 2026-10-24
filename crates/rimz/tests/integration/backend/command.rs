//! The shared subprocess engine behind every mux control command
//! (`rimz::mux::CommandSpec`). Subprocess behavior is integration-tier, so the
//! engine's spawn/exit/deadline contract is proven here, against real
//! processes (`true`/`false`/`sleep` — coreutils, present everywhere the
//! suite runs).

use std::time::Duration;

use rimz::mux::{CommandSpec, MuxErr};

#[test]
fn run_surfaces_success_and_nonzero_exit() {
    let output = CommandSpec::new("true").run().expect("`true` succeeds");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());

    let err = CommandSpec::new("false").run().expect_err("`false` fails");
    assert!(matches!(err, MuxErr::Command { .. }), "got: {err}");
}

#[test]
fn run_with_timeout_kills_a_hung_child() {
    let bound = Duration::from_millis(500);
    let started = std::time::Instant::now();
    let err = CommandSpec::new("sleep")
        .arg("30")
        .run_with_timeout(bound)
        .expect_err("the deadline fires");
    // The error names the caller's bound, not what was left of it.
    assert!(
        matches!(err, MuxErr::Timeout { timeout, .. } if timeout == bound),
        "got: {err}"
    );
    assert_eq!(
        err.to_string(),
        "multiplexer command `sleep 30` did not finish within 500ms; killed"
    );
    // The kill lands and the child is reaped promptly — well under the child's
    // own 30s sleep. Loose bound so a loaded CI box never flakes.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout path must not wait out the child",
    );
}
