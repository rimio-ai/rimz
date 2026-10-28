//! Performance-oriented integration checks. These assert bounded work or
//! single-flight behavior, not product semantics already covered elsewhere.

#[cfg(rimz_jemalloc)]
mod allocator;
mod concurrent_stops;
mod concurrent_writers;
mod consumer_enrichment;
mod enrichment_cadence;
mod fold_incremental;
mod parse_cache;
#[cfg(target_os = "linux")]
mod proc_sampling;
mod produce_budget;
mod sidebar_diff_stats;
mod spending_incremental;
mod store_bytes;
mod store_fsync;
#[cfg(target_os = "linux")]
mod work_log;

fn workspace_for_refresh(
    cursor: &mut rimz::sidebar::consumer::RollupCursor,
    state: &rimz::StatePaths,
    runtime: &rimz::RuntimePaths,
    frame: &rimz::sidebar::frame::PaneFrame,
) -> rimz::sidebar::enrich::WorkspaceSnapshot {
    let base = rimz::sidebar::consumer::rollup_snapshot(state, cursor).unwrap();
    let store = rimz::Store::open_existing(state.clone(), runtime.clone());
    rimz::sidebar::enrich::enrich_workspace(
        base,
        Some(frame),
        state,
        runtime,
        store.as_ref(),
        rimz::sidebar::enrich::FoldOpts {
            producing: false,
            fresh_roots: None,
            config: None,
            lanes: None,
            agent_projection: Default::default(),
        },
        &rimz::diag::DiagSink::disabled(),
    )
}
