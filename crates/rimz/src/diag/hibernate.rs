//! Durable evidence of wakes bypassing a timed-out hibernation lock.

use std::path::Path;

use crate::disk::paths::StatePaths;
use crate::ids::{PaneId, WorkspaceId};

pub(crate) fn wake_lock_timeout(
    paths: &StatePaths,
    pane_id: &PaneId,
    lock_path: &Path,
    withdrawn: usize,
) {
    #[derive(serde::Serialize)]
    #[serde(tag = "event", rename = "wake_lock_timeout")]
    struct Record<'a> {
        at: jiff::Timestamp,
        workspace_id: &'a WorkspaceId,
        pane_id: &'a PaneId,
        lock_path: &'a Path,
        withdrawn: usize,
    }
    crate::disk::rotating::append(
        &paths.audit_path("hibernate.log.jsonl"),
        crate::disk::retention::ROTATING_LOG_MAX_BYTES,
        &Record {
            at: jiff::Timestamp::now(),
            workspace_id: &paths.workspace_id,
            pane_id,
            lock_path,
            withdrawn,
        },
    );
}
