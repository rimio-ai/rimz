//! The in-process produce budget at fleet scale.
//!
//! The elder renderer runs [`rimz::sidebar::produce::produce_snapshot`] on its
//! fetch worker once per data tick (docs/internals/health/performance.md, the 2026-06
//! warm-producer pass). The contract: a warm steady-state produce — every
//! fork-bearing input pre-published fresh, the rollup folding O(new bytes)
//! through the worker's cursor — finishes far inside one data tick even with a
//! fleet-scale ledger and pane set, so the reconciling post never starves the
//! paint behind it. Companion to `compose_budget` in `sidebar_pane`,
//! which bounds the frame composition over the produced snapshot.

use std::time::{Duration, Instant};

use rimz::ledger::event_log;
use rimz::sidebar::consumer::RollupCursor;
use rimz::testkit::fleet::{
    SESSION_NAME, registered_lifecycle, seed_fleet_ledger, synthetic_panes,
};
use rimz::testkit::spawn_count;

use crate::common::Harness;

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;
const ROUNDS: u32 = 20;

#[test]
fn warm_produce_stays_inside_the_data_tick_at_fleet_scale() {
    let h = Harness::new();
    let paths = h.ledger.paths();
    seed_fleet_ledger(paths, FLEET, HISTORY_EVENTS).expect("seed event");
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
    // common case. Inputs re-publish outside the timed region.
    let mut elapsed = Duration::ZERO;
    let spawns_before = spawn_count();
    for round in 0..ROUNDS {
        let event = registered_lifecycle(&paths.workspace_id, round as usize % FLEET);
        event_log::append(&paths.events_log, &event).expect("append delta");
        h.publish_fresh_produce_inputs(SESSION_NAME, panes.clone());
        let start = Instant::now();
        let snapshot =
            rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
                .expect("warm produce");
        elapsed += start.elapsed();
        assert_eq!(snapshot.agents.len(), FLEET);
    }
    assert_eq!(
        spawn_count() - spawns_before,
        0,
        "a warm produce with every fork-bearing input pre-published forks no \
         subprocesses"
    );

    let per_produce = elapsed / ROUNDS;
    assert!(
        per_produce < Duration::from_millis(50),
        "one warm fleet-scale produce took {per_produce:?}; the 1s data tick \
         leaves no room for an envelope that slow beside the paint it feeds"
    );
}

#[test]
fn project_produce_over_stale_heavy_caches_forks_zero_subprocesses() {
    let h = Harness::new();
    let paths = h.ledger.paths();
    seed_fleet_ledger(paths, FLEET, HISTORY_EVENTS).expect("seed event");
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
