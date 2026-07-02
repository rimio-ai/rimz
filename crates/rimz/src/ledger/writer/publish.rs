use std::time::{Duration, SystemTime};

use tracing::warn;

use crate::feed::FeedItem;
use crate::harness::run::RunRecord;
use crate::ids::RequestId;
use crate::schema::event::EventEnvelope;

use super::super::{Ledger, LedgerErr, Result, StatePaths, event_log, lock, snapshot, wakeup};
use super::debounce::sync_log_debounced;

/// A publish failure caused by a corrupt event-log frame — the one failure
/// [`Ledger::repair_event_log`] heals, distinct from environment errors it
/// cannot.
fn publish_hit_corruption(err: &LedgerErr) -> bool {
    matches!(
        err,
        LedgerErr::Snapshot(snapshot::SnapshotErr::EventLog(log_err)) if log_err.is_corruption()
    )
}

/// Ceiling on how stale the published checkpoint may grow under sustained
/// writes. Consumers are woken per event and fold the log tail from their own
/// cursor, so the checkpoint is a cold-start accelerator, not the freshness
/// path.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);

/// Byte ceiling on the unpublished log tail: the gate forces an early
/// checkpoint once a cold reader would have to fold this much past the stamp,
/// whatever the stamp's age.
const PUBLISH_BYTE_BUDGET: u64 = 64 * 1024;

/// Stamp recording the last published checkpoint: the `LogExtent` the publish
/// reflected as content, the publish instant as mtime.
fn publish_stamp(paths: &StatePaths) -> std::path::PathBuf {
    paths.locks_dir.join("publish.stamp")
}

fn write_publish_stamp(paths: &StatePaths, extent: Option<event_log::LogExtent>) {
    let Some(extent) = extent else {
        return;
    };
    if let Ok(bytes) = serde_json::to_vec(&extent) {
        let _ = std::fs::write(publish_stamp(paths), bytes);
    }
}

/// The cheap pre-lock cadence gate: one open of the ~40-byte stamp decides
/// whether this tail pays the checkpoint.
fn publish_due(paths: &StatePaths) -> bool {
    use std::io::Read;

    // Missing or unreadable stamp: never published (or retracted) — due.
    let Ok(mut stamp) = std::fs::File::open(publish_stamp(paths)) else {
        return true;
    };
    let Some(age) = stamp
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        // Stamp mtime in the future (clock skew): treat the publish as due.
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
    else {
        return true;
    };
    if age >= PUBLISH_INTERVAL {
        // The interval alone decides the common due verdict — skip the
        // stamp-body read and the log stat.
        return true;
    }
    let mut bytes = Vec::with_capacity(64);
    let Some(extent) = stamp
        .read_to_end(&mut bytes)
        .ok()
        .and_then(|_| serde_json::from_slice::<event_log::LogExtent>(&bytes).ok())
    else {
        return true;
    };
    let log_len = std::fs::metadata(&paths.events_log)
        .map(|meta| meta.len())
        .unwrap_or(0);
    should_publish(age, extent.offset, log_len)
}

/// Pure gate core over the stamp's age and offset beside the live log length.
/// Due when the interval elapsed, the log shrank, or the unpublished tail
/// crossed the byte budget.
fn should_publish(age: Duration, stamp_offset: u64, log_len: u64) -> bool {
    age >= PUBLISH_INTERVAL
        || log_len < stamp_offset
        || log_len - stamp_offset >= PUBLISH_BYTE_BUDGET
}

/// Drop the publish stamp when the log it describes was swapped or cut —
/// rotation, identity rewrite, repair — so the next mutation's gate reads
/// "never published" instead of comparing offsets across two different files.
pub(super) fn retract_publish_stamp(paths: &StatePaths) {
    let _ = std::fs::remove_file(publish_stamp(paths));
}

impl Ledger {
    /// The off-lock tail every high-cadence mutator runs: the debounced group
    /// fdatasync, then the checkpoint publish when [`publish_due`] says the
    /// stamp aged out, the unpublished tail crossed the byte budget, or the
    /// log was swapped.
    pub(super) fn publish_snapshot_best_effort(&self) {
        self.publish_tail(false);
    }

    /// [`Self::publish_snapshot_best_effort`] without the cadence gate, for
    /// mutators that run rarely and whose callers read the checkpoint right
    /// after.
    pub(super) fn publish_snapshot_forced(&self) {
        self.publish_tail(true);
    }

    /// The one off-lock tail body behind both publish entry points: the
    /// debounced group fdatasync always runs; `force` decides whether the
    /// checkpoint skips the cadence gate.
    fn publish_tail(&self, force: bool) {
        sync_log_debounced(&self.inner.paths);
        if force || publish_due(&self.inner.paths) {
            self.publish_snapshot_now();
        }
    }

    /// Catch the snapshot caches up to the live log, after the workspace lock
    /// released and the wakeups went out. Serialized on its own advisory lock
    /// so concurrent publishers group-commit.
    fn publish_snapshot_now(&self) {
        let publish = || -> Result<()> {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;
            let snapshot = snapshot::rebuild(&self.inner.paths)?;
            write_publish_stamp(&self.inner.paths, snapshot.reflects_log);
            Ok(())
        };
        let Err(err) = publish() else {
            return;
        };
        if !publish_hit_corruption(&err) {
            warn!(error = %err, "snapshot publish failed after ledger commit");
            return;
        }
        // The publish closure released its lock above, so the repair's
        // workspace → publish acquisition nests in the canonical order.
        warn!(error = %err, "publish fold hit a corrupt event-log frame; repairing");
        if let Err(err) = self.repair_event_log() {
            warn!(error = %err, "event-log repair failed; run `rimz gc`");
            return;
        }
        // The repair republished when it cut; re-running covers the
        // found-intact race for the cost of an O(0-delta) fold.
        if let Err(err) = publish() {
            warn!(error = %err, "snapshot publish failed after event-log repair");
        }
    }

    pub(super) fn wake_per_request_best_effort(&self, item: &FeedItem) {
        if let Err(err) = wakeup::wake_per_request(&self.inner.runtime, item) {
            warn!(
                request_id = %item.request_id,
                error = %err,
                "per-request wakeup failed after ledger commit"
            );
        }
    }

    pub(super) fn wake_run_best_effort(&self, record: &RunRecord) {
        if let Err(err) = wakeup::wake_run(&self.inner.runtime, record) {
            warn!(
                run_id = %record.run_id,
                error = %err,
                "run wakeup failed after ledger commit"
            );
        }
    }

    pub(super) fn wake_sidebars_best_effort(&self, request_id: &RequestId) {
        if let Err(err) = wakeup::wake_sidebars(&self.inner.runtime) {
            warn!(
                request_id = %request_id,
                error = %err,
                "sidebar wakeup failed after ledger commit"
            );
        }
    }

    pub(super) fn wake_sidebars_hint_best_effort(&self) {
        if let Err(err) = wakeup::wake_sidebars(&self.inner.runtime) {
            warn!(error = %err, "sidebar wakeup failed after ledger commit");
        }
    }

    pub(super) fn wake_sidebars_for_event_best_effort(&self, event: &EventEnvelope) {
        if let Err(err) = wakeup::wake_sidebars_for_event(&self.inner.runtime, event) {
            warn!(
                event_id = %event.event_id,
                error = %err,
                "sidebar wakeup failed after ledger event commit"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_gate_boundaries() {
        for (label, age, stamp_offset, log_len, expected) in [
            (
                "mid-interval with nothing unpublished",
                PUBLISH_INTERVAL - Duration::from_millis(1),
                100,
                100,
                false,
            ),
            ("interval boundary", PUBLISH_INTERVAL, 100, 100, true),
            (
                "under byte budget",
                Duration::ZERO,
                100,
                100 + PUBLISH_BYTE_BUDGET - 1,
                false,
            ),
            (
                "byte budget boundary",
                Duration::ZERO,
                100,
                100 + PUBLISH_BYTE_BUDGET,
                true,
            ),
            ("shrunken log after rotation", Duration::ZERO, 100, 99, true),
            ("fresh tail below budget", Duration::ZERO, 100, 101, false),
        ] {
            assert_eq!(
                should_publish(age, stamp_offset, log_len),
                expected,
                "{label}",
            );
        }
    }
}
