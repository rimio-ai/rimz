//! Elder-owned scheduled-message wakeups.
//!
//! The room host reads only the wake cache and spawns the hidden `rimz message sweep` helper
//! with its workspace id and mux in argv; store reads and writes stay in that helper. The
//! refresh loop retains the last spawned stamp and its pass time, bounding unchanged-stamp
//! retries to one prompt delivery window without delaying a changed due stamp.

use std::ffi::OsString;
use std::path::Path;

use jiff::{Timestamp, Zoned};

use crate::RuntimePaths;
use crate::ids::{MuxName, WorkspaceId};
use crate::store::message::MessageBody;

pub(crate) fn wake_due_messages(
    runtime: &RuntimePaths,
    mux: MuxName,
    now: &Zoned,
    last_spawn: &mut Option<(Timestamp, Timestamp)>,
) {
    let path = runtime.message_wake_path();
    let stamp = read_stamp(&path);
    let now = now.timestamp();
    if !should_wake(stamp, now, *last_spawn) {
        return;
    }
    *last_spawn = stamp.map(|stamp| (stamp, now));
    spawn_message_sweep(runtime, mux);
}

fn should_wake(
    stamp: Option<Timestamp>,
    now: Timestamp,
    last_spawn: Option<(Timestamp, Timestamp)>,
) -> bool {
    stamp.is_some_and(|stamp| {
        stamp <= now
            && last_spawn.is_none_or(|(last_stamp, spawned_at)| {
                stamp != last_stamp || now >= spawned_at + MessageBody::Prompt.delivery_window()
            })
    })
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
            assert_eq!(should_wake(stamp, now, None), expected, "{stamp:?}");
        }
    }

    #[test]
    fn unchanged_stamp_waits_one_delivery_window_but_changed_stamp_wakes_at_once() {
        let now = Timestamp::from_second(100).unwrap();
        let stamp = now - std::time::Duration::from_secs(1);
        let window = crate::store::message::MessageBody::Prompt.delivery_window();
        let mut last_spawn = None;
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for second in 0..=window.as_secs() {
            let pass_now = now + std::time::Duration::from_secs(second);
            let wake = should_wake(Some(stamp), pass_now, last_spawn);
            actual.push(wake);
            expected.push(second == 0 || second == window.as_secs());
            if wake {
                last_spawn = Some((stamp, pass_now));
            }
        }
        let next_pass = now + window + std::time::Duration::from_secs(1);
        let changed_stamp = stamp - std::time::Duration::from_secs(1);
        let wake = should_wake(Some(changed_stamp), next_pass, last_spawn);
        actual.push(wake);
        expected.push(true);
        if wake {
            last_spawn = Some((changed_stamp, next_pass));
        }
        for (stamp, pass_now, wake) in [
            (None, next_pass, false),
            (Some(next_pass + window), next_pass, false),
            (Some(changed_stamp), next_pass, false),
            (Some(changed_stamp), next_pass + window, true),
        ] {
            actual.push(should_wake(stamp, pass_now, last_spawn));
            expected.push(wake);
        }
        assert_eq!(actual, expected);
    }
}
