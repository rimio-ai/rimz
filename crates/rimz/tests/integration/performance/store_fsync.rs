//! Fsync discipline of the relaxed write path.
//!
//! Per-record event-log fsyncs were the write path's dominant latency, so
//! the contract (docs/internals/store/store.md): an event append performs zero
//! fsyncs of its own — durability rides the off-lock write tail's group
//! fdatasync, debounced to at most one per [`LOG_SYNC_INTERVAL`], which one
//! writer pays for the whole fleet. Every fsync syscall funnels through
//! `store::atomic` (CI grep), so its testkit counter sees them all.

use std::time::{Duration, Instant, SystemTime};

use rimz::EventEnvelope;
use rimz::store::atomic::testkit::fsync_count;

use crate::common::Harness;

fn lifecycle(h: &Harness, agent_id: &str) -> EventEnvelope {
    crate::common::lifecycle_event(h, "rimz-perf", "SessionStart", agent_id)
}

fn log_sync_stamp(h: &Harness) -> std::path::PathBuf {
    h.store.paths().locks_dir.join("log-sync.stamp")
}

#[test]
fn warm_appends_perform_zero_fsyncs() {
    // Steady-state appends are write()-only, and published snapshots are
    // cache-class. Durability rides the debounced group sync and the audit log
    // alone.
    let h = Harness::new();
    h.store
        .append_event(&lifecycle(&h, "warmup"))
        .expect("warmup");

    // Pin the group-sync debounce mid-interval for the measured block.
    std::fs::write(log_sync_stamp(&h), b"").expect("touch stamp");
    let before = fsync_count();
    h.store
        .append_event(&lifecycle(&h, "steady"))
        .expect("steady append");
    h.store
        .append_event(&lifecycle(&h, "steady-2"))
        .expect("second steady append");
    assert_eq!(
        fsync_count() - before,
        0,
        "steady-state appends perform no fsync anywhere — critical section or tail"
    );
}

#[test]
fn a_write_burst_pays_one_group_sync_per_interval() {
    let h = Harness::new();
    h.store
        .append_event(&lifecycle(&h, "warmup"))
        .expect("warmup");

    // Backdate the stamp so the burst opens with the interval elapsed,
    // without sleeping through it.
    let aged = SystemTime::now() - Duration::from_secs(2);
    std::fs::File::options()
        .write(true)
        .open(log_sync_stamp(&h))
        .expect("open stamp")
        .set_modified(aged)
        .expect("age stamp");

    let before = fsync_count();
    let start = Instant::now();
    for i in 0..20 {
        h.store
            .append_event(&lifecycle(&h, &format!("agent-{i}")))
            .expect("burst append");
    }
    let elapsed = start.elapsed();
    let syncs = fsync_count() - before;
    // One sync per elapsed interval: exactly 1 on any healthy runner (the
    // burst completes well inside the window), bounded by the intervals a
    // stalled run could legally straddle.
    let ceiling = elapsed.as_secs() + 1;
    assert!(
        (1..=ceiling).contains(&syncs),
        "an overdue interval fires one group fdatasync for the whole burst \
         (saw {syncs} across {elapsed:?}); every other append rides the page \
         cache"
    );
}
