use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tracing::warn;

use super::super::{StatePaths, atomic};

/// How often the write path is willing to pay the dead-owner sweep. Read-side
/// expel hides a dead-owner item from runtime views the instant it dies, so
/// the sweep only owes the durable `abandoned` record within this window.
const ABANDON_SWEEP_INTERVAL: Duration = Duration::from_secs(2);

/// Stamp recording the last dead-owner sweep. Lives beside the workspace lock
/// so feed-dir scans (item lists, gc's history classification) never see it.
pub(super) fn abandon_sweep_stamp(paths: &StatePaths) -> PathBuf {
    paths.locks_dir.join("abandon-sweep.stamp")
}

/// Age of a debounce stamp's mtime. `None` when the stamp is missing or
/// unreadable, or its mtime sits in the future (clock skew) — every gate
/// reads `None` as due, erring toward one redundant run, never a stale skip.
pub(super) fn stamp_age(path: &std::path::Path) -> Option<Duration> {
    let modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Best-effort: a failed touch only means the next write runs the gated task
/// again.
pub(super) fn touch_stamp(path: &std::path::Path) {
    let _ = std::fs::write(path, b"");
}

pub(super) fn abandon_sweep_due(paths: &StatePaths) -> bool {
    stamp_age(&abandon_sweep_stamp(paths)).is_none_or(|age| age >= ABANDON_SWEEP_INTERVAL)
}

/// How long appended event-log bytes may ride the page cache before a write
/// tail forces them down. Bounds power-cut loss to about a second of trailing
/// events under sustained load.
const LOG_SYNC_INTERVAL: Duration = Duration::from_secs(1);

/// Stamp recording the last event-log group sync. Lives beside the workspace
/// lock with the other write-path debounce stamps.
fn log_sync_stamp(paths: &StatePaths) -> PathBuf {
    paths.locks_dir.join("log-sync.stamp")
}

fn log_sync_due(paths: &StatePaths) -> bool {
    stamp_age(&log_sync_stamp(paths)).is_none_or(|age| age >= LOG_SYNC_INTERVAL)
}

/// Group-commit the relaxed event-log appends at most once per
/// [`LOG_SYNC_INTERVAL`]. One fdatasync flushes the inode's dirty pages
/// regardless of which process wrote them, so a single writer per interval
/// makes the whole fleet's appends durable.
pub(super) fn sync_log_debounced(paths: &StatePaths) {
    if !log_sync_due(paths) {
        return;
    }
    match atomic::sync_file_data(&paths.events_log) {
        Ok(()) => touch_stamp(&log_sync_stamp(paths)),
        // A rotation can rename the log away between the append and this
        // tail; its pre-rename sync already made those bytes durable.
        Err(atomic::AtomicErr::Io { ref source, .. })
            if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => warn!(error = %err, "event-log group sync failed; the next write retries"),
    }
}
