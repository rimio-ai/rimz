//! History-independence of the warm rollup fold.
//!
//! Every sidebar tab folds the event log on every wakeup, and tens of agents
//! push tens of events per second. The contract
//! (docs/internals/health/performance.md): a warm fold reads only the bytes
//! appended since its held base — one frame per event, never the log — so
//! per-tab work per event stays O(frame) while the log grows without bound.
//! Companion to `spending_incremental`, which proves the same shape for the
//! transcript walk.

use rimz::ledger::event_log::{self, testkit::bytes_read};
use rimz::ledger::snapshot::RollupCursor;
use rimz::testkit::fleet::{
    SESSION_NAME, registered_lifecycle, seed_fleet_ledger, synthetic_panes,
};

use crate::common::Harness;

const HISTORY_EVENTS: usize = 3_000;
const FLEET: usize = 30;

#[test]
fn delta_fold_is_o_new_bytes() {
    let h = Harness::new();
    let paths = h.ledger.paths();
    // Seed history through the raw log API: mutator tails would publish per
    // append, and the subject here is the reader's fold alone.
    seed_fleet_ledger(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let mut cursor = RollupCursor::new();
    let cold_before = bytes_read();
    let (cold_extent, _) = cursor.fold(paths).expect("cold fold");
    let cold_bytes = bytes_read() - cold_before;
    assert_eq!(cold_extent.offset, log_len, "the cold fold reaches the end");
    assert_eq!(cold_bytes, log_len, "a cold fold reads the whole history");

    // One event lands; the warm fold pays for that frame alone.
    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    let warm_before = bytes_read();
    let (warm_extent, agents) = cursor.fold(paths).expect("warm fold");
    let warm_bytes = bytes_read() - warm_before;

    assert_eq!(warm_extent.offset, log_len + appended);
    assert_eq!(
        warm_bytes, appended,
        "a warm fold reads exactly the appended frame, independent of the \
         {cold_bytes}-byte history"
    );
    assert_eq!(agents.len(), FLEET, "the fold still lands the merged view");
}

/// The full produce pipeline inherits the cursor contract end to end: a
/// second [`rimz::sidebar::produce::produce_snapshot`] on one cursor reads
/// exactly the bytes appended since the first — the elder fetch worker's
/// steady state, where one warm cursor serves the fast lane and the produce
/// alike. Every fork-bearing enrichment input is pre-published fresh
/// ([`Harness::publish_fresh_produce_inputs`]), so the produce pays no mux
/// and no subprocess, and the byte counter isolates the rollup read.
#[test]
fn warm_produce_folds_o_new_bytes() {
    let h = Harness::new();
    let paths = h.ledger.paths();
    seed_fleet_ledger(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: None,
        heavy_lanes: rimz::sidebar::produce::HeavyLaneMode::Refresh,
    };
    let mut cursor = RollupCursor::new();

    h.publish_fresh_produce_inputs(SESSION_NAME, synthetic_panes(1));
    let cold_before = bytes_read();
    let cold =
        rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
            .expect("cold produce");
    let cold_bytes = bytes_read() - cold_before;
    assert_eq!(
        cold_bytes, log_len,
        "a cold produce folds the whole history"
    );
    assert_eq!(cold.agents.len(), FLEET);

    // One event lands; the warm produce pays for that frame alone.
    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    h.publish_fresh_produce_inputs(SESSION_NAME, synthetic_panes(1));
    let warm_before = bytes_read();
    let warm =
        rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
            .expect("warm produce");
    let warm_bytes = bytes_read() - warm_before;
    assert_eq!(
        warm_bytes, appended,
        "a warm produce reads exactly the appended frame, independent of the \
         {cold_bytes}-byte history"
    );
    assert_eq!(
        warm.agents.len(),
        FLEET,
        "the produce lands the merged view"
    );
}
