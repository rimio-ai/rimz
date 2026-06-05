//! The fleet spending walk: the `SPENDING_TTL`-gated transcript-history walk
//! and the `value_tally` fold it feeds.

use std::path::PathBuf;

use crate::sidebar::snapshot::unix_now_ms;

/// Walk every provider's transcript history into a fleet-wide and per-provider
/// [`Spending`](crate::agents::spending::Spending), publishing the stamped
/// result to `provider-spending.json` — the cache consumer tabs read instead of
/// walking, and the producer's own gate: a stamp younger than
/// [`SPENDING_TTL`](crate::agents::spending::SPENDING_TTL) serves the published
/// totals with zero transcript IO — no adapter discovery, no `spending.json`
/// cursor read, no price-book load.
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
) -> crate::agents::spending::Spending {
    use crate::agents::pricing;
    use crate::agents::spending::{
        Spending, compute_spending, read_provider_spending_cache, read_spending_cache,
        unix_secs_now, write_provider_spending_cache, write_spending_cache,
    };
    use crate::agents::{ADAPTERS, AgentAdapter};

    let provider_path = runtime.root.join("provider-spending.json");
    let now_ms = unix_now_ms();
    // Fresh stamp: the published walk is young enough — serve it back with the
    // same single small read a consumer tab pays.
    let published = read_provider_spending_cache(&provider_path);
    if published.is_fresh(now_ms) {
        return published.spending;
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
    if files.is_empty() {
        // Stamp the empty result too: an agentless machine must not re-run the
        // (empty) discovery readdirs every tick.
        write_provider_spending_cache(&provider_path, now_ms, &Spending::default());
        return Spending::default();
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
    write_provider_spending_cache(&provider_path, now_ms, &spending);
    spending
}

/// Attach the fleet `value_tally` to the snapshot — the JSONL today / month /
/// all-time pile read by both the cockpit's today figure and the bottom value
/// corner; `None` when nothing has ever been recorded. The per-provider breakdown
/// is folded into the dashboard panels separately (see `with_provider_aggregates`).
pub(super) fn apply_spending(
    snapshot: &mut crate::SidebarSnapshot,
    spending: &crate::agents::spending::Spending,
) {
    snapshot.value_tally = (!spending.total.is_zero()).then(|| spending.total.clone());
}
