//! The fleet spending walk: the `SPENDING_TTL`-gated transcript-history walk
//! feeding the enrichment spine's `value_tally` and per-provider dashboard
//! folds.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::sidebar::cache::unix_now_ms;

/// Walk every provider's transcript history into a fleet-wide and per-provider
/// [`Spending`](crate::agents::spending::Spending), publishing the stamped
/// result to `provider-spending.json` — the cache consumer tabs read instead of
/// walking, and the producer's own gate: a stamp younger than
/// [`SPENDING_TTL`](crate::agents::spending::SPENDING_TTL) serves the published
/// totals with zero transcript IO — no adapter discovery, no `spending.json`
/// cursor read, no price-book load. Each fresh publish also stamps
/// `live_costs` — the live sessions' statusline costs at this exact moment —
/// as the baselines the cockpit's live overlay measures overshoot against
/// until the next walk; a served-back cache keeps the baselines and stamp of
/// the publish that captured them.
///
/// The stale walk reads the per-workspace `spending.json` cache, refreshes only
/// files whose mtime changed, then writes back if anything was updated, and
/// loads the price book (a TTL-gated remote refresh) so Codex's token counts
/// become dollars. Best-effort: a read/write or fetch failure degrades
/// gracefully to the cached or embedded data.
///
/// Every registered adapter is discovered fleet-wide
/// ([`transcript_files`](crate::agents::AgentAdapter::transcript_files)) so each
/// counts on the same footing, and the dashboard panel and fleet ledger read
/// one provider's spend the same way regardless of which project it ran in.
pub(super) fn compute_fleet_spending(
    runtime: &crate::RuntimePaths,
    live_costs: impl FnOnce() -> BTreeMap<String, f64>,
) -> crate::agents::spending::ProviderSpendingCache {
    use crate::agents::pricing;
    use crate::agents::spending::{
        ProviderSpendingCache, Spending, compute_spending, read_provider_spending_cache,
        read_spending_cache, unix_secs_now, write_provider_spending_cache, write_spending_cache,
    };
    use crate::agents::{ADAPTERS, AgentAdapter};

    let provider_path = runtime.root.join("provider-spending.json");
    let now_ms = unix_now_ms();
    // Fresh stamp: the published walk is young enough — serve it back with the
    // same single small read a consumer tab pays.
    let published = read_provider_spending_cache(&provider_path);
    if published.is_fresh(now_ms) {
        return published;
    }

    // Tag each file with its adapter at discovery — the source knows the kind,
    // so pricing/bucketing never has to guess it from the path.
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = ADAPTERS
        .iter()
        .flat_map(|adapter| {
            adapter
                .transcript_files()
                .into_iter()
                .map(move |file| (*adapter, file))
        })
        .collect();
    let live_costs = live_costs();
    if files.is_empty() {
        // Stamp the empty result too: an agentless machine must not re-run the
        // (empty) discovery readdirs every tick.
        write_provider_spending_cache(
            &provider_path,
            now_ms,
            &Spending::default(),
            live_costs.clone(),
        );
        return ProviderSpendingCache {
            refreshed_at_ms: now_ms,
            live_cost_baselines: live_costs,
            spending: Spending::default(),
        };
    }

    let cache_path = runtime.root.join("spending.json");
    let mut cache = read_spending_cache(&cache_path);
    // The price book exists only to price the walk, so its load (and TTL-gated
    // remote refresh) rides the stale arm with it.
    let prices = pricing::load_for_spending(&runtime.root.join("pricing-cache.json"));
    let spending = compute_spending(&files, &mut cache, &prices, unix_secs_now());
    if cache.dirty {
        write_spending_cache(&cache_path, &cache);
    }
    write_provider_spending_cache(&provider_path, now_ms, &spending, live_costs.clone());
    ProviderSpendingCache {
        refreshed_at_ms: now_ms,
        live_cost_baselines: live_costs,
        spending,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use crate::RuntimePaths;
    use crate::agents::spending::{Spending, write_provider_spending_cache};
    use crate::ids::WorkspaceId;
    use crate::sidebar::cache::unix_now_ms;

    use super::compute_fleet_spending;

    #[test]
    fn fresh_publish_returns_without_collecting_live_costs() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");
        let published_at = unix_now_ms();
        write_provider_spending_cache(
            &runtime.root.join("provider-spending.json"),
            published_at,
            &Spending::default(),
            BTreeMap::from([("agent-1".to_owned(), 1.23)]),
        );

        let collected = Cell::new(false);
        let cache = compute_fleet_spending(&runtime, || {
            collected.set(true);
            BTreeMap::new()
        });

        assert_eq!(cache.refreshed_at_ms, published_at);
        assert_eq!(cache.live_cost_baselines["agent-1"], 1.23);
        assert!(
            !collected.get(),
            "fresh spending publish should not allocate live-cost baselines"
        );
    }
}
