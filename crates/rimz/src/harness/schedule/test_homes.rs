//! The private-home re-exec every unit test that writes the machine's loop state shares, and
//! the fire mark fixture of the schedule's unit tests. A fixture that opens a `Store` stays in
//! a `tests.rs`: `cargo xtask invariants` reads this file as production code.

use std::process::Command;

use jiff::Timestamp;

use super::task_record::{self, FireMark};
use crate::config::LaunchKind;
use crate::ids::FireId;

/// Runs test `name` again in a child process with private homes, for a test that writes the
/// machine's loop state (a created run, the task record): every test process shares the
/// unit-test home. Returns whether this is the parent, done.
pub(crate) fn reexec_with_private_homes(name: &str) -> bool {
    const CHILD: &str = "RIMZ_TEST_PRIVATE_HOMES_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    // Under /tmp so the runtime root's socket paths stay short.
    let home = tempfile::Builder::new()
        .prefix("rimz-private-homes-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, "1")
        .env("HOME", home.path())
        .env("CLAUDE_CONFIG_DIR", home.path().join("claude"))
        .env_remove("RIMZ_CLAUDE_SETTINGS");
    for (key, leaf) in [
        ("RIMZ_HOME", ".rimz"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let path = home.path().join(leaf);
        std::fs::create_dir_all(&path).unwrap();
        command
            .env(key, &path)
            .env(format!("RIMZ_UNIT_TEST_{key}"), path);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{name} failed in its re-exec: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    true
}

pub(super) fn dead_mark(fire: FireId, child: bool) -> FireMark {
    FireMark {
        fire,
        at: Timestamp::now(),
        booked_by: Vec::new(),
        once: false,
        run: None,
        scope: crate::store::workflow::ScopeKind::Worktree,
        result: None,
        kind: Some(LaunchKind::Agent),
        target: None,
        manual: false,
        opener: None,
        child: child.then_some(task_record::ProcessMark {
            pid: 1,
            start: None,
        }),
    }
}
