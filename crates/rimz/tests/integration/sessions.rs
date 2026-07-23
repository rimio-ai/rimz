//! Integration coverage for the `rimz sessions` entry guards.

use crate::common::Env;

#[test]
fn sessions_non_tty_lists_live_rooms_and_exits_nonzero() {
    let env = Env::new();
    let output = env.rimz().arg("sessions").output().expect("run sessions");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`rimz sessions` needs an interactive terminal"),
        "{stderr}"
    );
    assert!(stderr.contains("Live RimZ sessions:"), "{stderr}");
    assert!(stderr.contains("(none)"), "{stderr}");
}

#[test]
fn sessions_refuses_to_nest_inside_a_mux() {
    let env = Env::new();
    let output = env
        .rimz()
        .arg("sessions")
        .env("TMUX", "fixture")
        .output()
        .expect("run sessions inside tmux");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already inside a multiplexer"), "{stderr}");
    assert!(stderr.contains("use `rimz attach`"), "{stderr}");
    assert!(!stderr.contains("Live RimZ sessions:"), "{stderr}");
}
