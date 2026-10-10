//! Elder-owned scheduled-message wakeups.
//!
//! The room host keeps time for queued messages with a future delivery floor while a room is open. The host reads only the wake cache and spawns the hidden `rimz message sweep` helper with its workspace id and mux in argv; store reads and writes stay in that helper.

use std::ffi::OsString;
use std::path::Path;

use jiff::{Timestamp, Zoned};

use crate::RuntimePaths;
use crate::ids::{MuxName, WorkspaceId};
use crate::message::deliver::wake_stamp_path;

pub(crate) fn wake_due_messages(runtime: &RuntimePaths, mux: MuxName, now: &Zoned) {
    let path = wake_stamp_path(runtime);
    if !should_wake(read_stamp(&path), now.timestamp()) {
        return;
    }
    spawn_message_sweep(runtime, mux);
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

fn sweep_args(workspace_id: &WorkspaceId, mux: MuxName) -> Vec<OsString> {
    [
        "--mux",
        mux.as_str(),
        "message",
        "sweep",
        "--workspace-id",
        workspace_id.as_str(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn spawn_message_sweep(runtime: &RuntimePaths, mux: MuxName) {
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        "sidebar: sweeping scheduled messages",
    );
    if let Err(err) = crate::child_process::spawn_detached_rimz(
        runtime,
        sweep_args(&runtime.workspace_id, mux),
        "message-sweep",
    ) {
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
    fn sweep_args_names_room_and_mux() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/room"));
        let actual = [MuxName::Zellij, MuxName::Tmux].map(|mux| sweep_args(&workspace_id, mux));
        let expected = ["zellij", "tmux"].map(|mux| {
            [
                "--mux",
                mux,
                "message",
                "sweep",
                "--workspace-id",
                workspace_id.as_str(),
            ]
            .map(OsString::from)
            .to_vec()
        });
        assert_eq!(actual, expected);
    }

    #[test]
    fn wake_decision_fires_only_for_due_stamp() {
        let now = Timestamp::from_second(100).unwrap();
        let cases = [
            (None, false),
            (Some(Timestamp::from_second(99).unwrap()), true),
            (Some(Timestamp::from_second(100).unwrap()), true),
            (Some(Timestamp::from_second(101).unwrap()), false),
        ];
        for (stamp, expected) in cases {
            assert_eq!(should_wake(stamp, now), expected, "{stamp:?}");
        }
    }
}
