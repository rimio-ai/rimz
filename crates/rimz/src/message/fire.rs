//! Elder-owned scheduled-message wakeups.
//!
//! The elected sidebar elder keeps time for queued messages with a future
//! delivery floor while a room is open. The elder reads only the wake cache and
//! spawns the hidden `rimz message sweep` helper; store reads and writes stay in
//! that helper.

use std::path::Path;
use std::process::{Command, Stdio};

use jiff::{Timestamp, Zoned};

use crate::RuntimePaths;
use crate::message::deliver::wake_stamp_path;

pub(crate) fn wake_due_messages(runtime: &RuntimePaths, now: &Zoned) {
    let path = wake_stamp_path(runtime);
    if !should_wake(read_stamp(&path), now.timestamp()) {
        return;
    }
    spawn_message_sweep(runtime);
}

fn should_wake(stamp: Option<Timestamp>, now: Timestamp) -> bool {
    stamp.is_some_and(|stamp| stamp <= now)
}

fn read_stamp(path: &Path) -> Option<Timestamp> {
    let Ok(bytes) = std::fs::read(path) else {
        return None;
    };
    serde_json::from_slice::<Option<Timestamp>>(&bytes)
        .ok()
        .flatten()
}

fn spawn_message_sweep(runtime: &RuntimePaths) {
    let exe = crate::proc::rimz_exe();
    let mut cmd = Command::new(exe);
    if let Ok(root) = std::env::var(crate::workspace::ENV_PROJECT_ROOT) {
        cmd.args(["--root", &root]);
    }
    cmd.args(["message", "sweep"])
        .current_dir(&runtime.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        "sidebar: sweeping scheduled messages",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "message-sweep") {
        // The CWD anchor clears gc'd-worktree ENOENT; a bad RIMZ_BIN/PATH stays
        // an environment fact. Keep it at debug! so Sentry ignores it, and the
        // next elder tick retries.
        tracing::debug!(
            tags.operation = "message.fire.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn scheduled-message sweep",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_stamp_wakes() {
        let now = Timestamp::from_second(100).unwrap();
        assert!(should_wake(Some(now), now));
        assert!(should_wake(Some(Timestamp::from_second(99).unwrap()), now));
    }

    #[test]
    fn future_stamp_waits() {
        let now = Timestamp::from_second(100).unwrap();
        assert!(!should_wake(
            Some(Timestamp::from_second(101).unwrap()),
            now
        ));
    }

    #[test]
    fn missing_stamp_waits() {
        assert!(!should_wake(None, Timestamp::from_second(100).unwrap()));
    }
}
