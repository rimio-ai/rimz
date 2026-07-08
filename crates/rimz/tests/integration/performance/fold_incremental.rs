//! History-independence of the warm rollup fold.
//!
//! Every sidebar tab folds the event log on every wakeup, and tens of agents
//! push tens of events per second. The contract
//! (docs/internals/performance.md): a warm fold reads only the bytes
//! appended since its held base — one frame per event, never the log — so
//! per-tab work per event stays O(frame) while the log grows without bound.
//! Companion to `spending_incremental`, which proves the same shape for the
//! transcript walk.

use rimz::store::event_log::{self, testkit::bytes_read};
use rimz::store::snapshot::RollupCursor;
use rimz::testkit::fleet::{SESSION_NAME, registered_lifecycle, seed_fleet_store, synthetic_panes};

use crate::common::Harness;

const HISTORY_EVENTS: usize = 3_000;
const FLEET: usize = 30;

#[test]
fn delta_fold_is_o_new_bytes() {
    let h = Harness::new();
    let paths = h.store.paths();
    // Seed history through the raw log API: mutator tails would publish per
    // append, and the subject here is the reader's fold alone.
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let mut cursor = RollupCursor::new();
    let cold_before = bytes_read();
    let (cold_extent, _, _) = cursor.fold(paths).expect("cold fold");
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
    let (warm_extent, agents, _) = cursor.fold(paths).expect("warm fold");
    let warm_bytes = bytes_read() - warm_before;

    assert_eq!(warm_extent.offset, log_len + appended);
    assert_eq!(
        warm_bytes, appended,
        "a warm fold reads exactly the appended frame, independent of the \
         {cold_bytes}-byte history"
    );
    assert_eq!(agents.len(), FLEET, "the fold still lands the merged view");
}

#[test]
fn runtime_projection_uses_persisted_rollup_delta() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    h.store
        .append_event(&registered_lifecycle(&paths.workspace_id, 0))
        .expect("publish rollup");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let warm_before = bytes_read();
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("runtime projection");
    let warm_bytes = bytes_read() - warm_before;
    assert_eq!(projection.agents.len(), FLEET);
    assert_eq!(
        warm_bytes, 0,
        "a fresh persisted rollup keeps runtime projection off the {log_len}-byte history"
    );

    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    let delta_before = bytes_read();
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("runtime projection after append");
    let delta_bytes = bytes_read() - delta_before;
    assert_eq!(projection.agents.len(), FLEET);
    assert_eq!(
        delta_bytes, appended,
        "runtime projection reads exactly the appended frame after its persisted base"
    );
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
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: rimz::diag::DiagSink::disabled(),
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

#[test]
fn warm_auto_continue_tick_reads_no_event_log_history() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();
    let mut cursor = RollupCursor::new();

    let cold_before = bytes_read();
    let cold = auto_continue_tick(&mut cursor, paths, &h.runtime_paths);
    let cold_bytes = bytes_read() - cold_before;
    assert_eq!(
        cold_bytes, log_len,
        "a cold auto-continue tick folds the log once through the rollup"
    );
    assert_eq!(cold.agents.len(), FLEET);

    for tick in 0..3 {
        let warm_before = bytes_read();
        let warm = auto_continue_tick(&mut cursor, paths, &h.runtime_paths);
        let warm_bytes = bytes_read() - warm_before;
        assert_eq!(
            warm_bytes, 0,
            "unchanged-log auto-continue tick {tick} must not read event-log history"
        );
        assert_eq!(warm.agents.len(), FLEET);
    }

    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    let append_before = bytes_read();
    let warm = auto_continue_tick(&mut cursor, paths, &h.runtime_paths);
    let append_bytes = bytes_read() - append_before;
    assert_eq!(
        append_bytes, appended,
        "after one append, auto-continue tick reads only the appended frame"
    );
    assert_eq!(warm.agents.len(), FLEET);
}

fn auto_continue_tick(
    cursor: &mut RollupCursor,
    state: &rimz::StatePaths,
    runtime: &rimz::RuntimePaths,
) -> rimz::SidebarSnapshot {
    let base = rimz::store::snapshot::build_with_cursor(state, cursor).expect("rollup");
    let mut config = rimz::config::MachineConfig::default();
    config.resume.auto_continue = true;
    rimz::sidebar::enrich::enrich(
        base,
        None,
        runtime,
        Some(&state.messages_dir),
        None,
        rimz::sidebar::enrich::FoldOpts {
            producing: true,
            fresh_roots: None,
            config: Some(std::sync::Arc::new(config)),
            lanes: None,
        },
        &rimz::diag::DiagSink::disabled(),
    )
}
