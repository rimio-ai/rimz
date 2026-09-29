//! The in-process produce cost at fleet scale.
//!
//! The elder renderer runs `rimz::sidebar::produce::produce_workspace_snapshot` on its
//! fetch worker once per data tick (docs/internals/performance.md, the 2026-06
//! warm-producer pass). The contract: a warm steady-state produce — every
//! fork-bearing input pre-published fresh, the rollup folding O(new bytes)
//! through the worker's cursor — forks no subprocess even with a fleet-scale
//! store and pane set. These tests pin that count; the wall-clock side lives in
//! the `fleet::produce_warm` bench (`cargo xtask perf`), because a timing
//! budget checked while nextest runs the whole suite in parallel fails on load,
//! not on regressions.

use rimz::sidebar::consumer::RollupCursor;
use rimz::store::event_log;
use rimz::testkit::fleet::{SESSION_NAME, registered_lifecycle, seed_fleet_store, synthetic_panes};
use rimz::testkit::spawn_count;

use crate::common::Harness;

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;
const ROUNDS: u32 = 20;

#[test]
fn warm_fleet_produce_forks_zero_subprocesses() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let panes = synthetic_panes(FLEET);
    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: rimz::diag::DiagSink::disabled(),
    };
    let mut cursor = RollupCursor::new();

    // The cold produce pays the one-time history fold; uncounted, like the
    // first frame after attach.
    h.publish_fresh_produce_inputs(SESSION_NAME, panes.clone());
    rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
        .expect("cold produce");

    // Steady state: one delta per tick, every stamp young — the elder's
    // common case. Inputs re-publish before each produce.
    let spawns_before = spawn_count();
    for round in 0..ROUNDS {
        let event = registered_lifecycle(&paths.workspace_id, round as usize % FLEET);
        event_log::append(&paths.events_log, &event).expect("append delta");
        h.publish_fresh_produce_inputs(SESSION_NAME, panes.clone());
        let snapshot =
            rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
                .expect("warm produce");
        assert_eq!(snapshot.agents.len(), FLEET);
    }
    assert_eq!(
        spawn_count() - spawns_before,
        0,
        "a warm produce with every fork-bearing input pre-published forks no \
         subprocesses"
    );
}

#[test]
fn project_produce_over_stale_heavy_caches_forks_zero_subprocesses() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    h.publish_fresh_produce_inputs(SESSION_NAME, synthetic_panes(FLEET));
    let _ = std::fs::remove_file(h.runtime_paths.shared_provider_spending_path());
    let _ = std::fs::remove_file(h.runtime_paths.shared_accounts_path());
    let _ = std::fs::remove_file(h.runtime_paths.diff_stats_path());

    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: rimz::diag::DiagSink::disabled(),
    };
    let mut cursor = RollupCursor::new();

    let spawns_before = spawn_count();
    let snapshot =
        rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
            .expect("project produce");
    assert_eq!(snapshot.agents.len(), FLEET);
    assert_eq!(
        spawn_count() - spawns_before,
        0,
        "produce projects missing/stale heavy caches and never refreshes them inline"
    );
}
