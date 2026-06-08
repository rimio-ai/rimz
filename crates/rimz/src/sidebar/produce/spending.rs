//! The fleet spending walk: the `SPENDING_TTL`-gated transcript-history walk
//! feeding the enrichment spine's `value_tally` and per-provider dashboard
//! folds.

use std::path::PathBuf;
use std::time::Duration;

use crate::sidebar::cache::unix_now_ms;

const SPENDING_WAIT_STEP: Duration = Duration::from_millis(20);
const SPENDING_WAIT_STEPS: u32 = 15;

/// Walk every provider's transcript history into a fleet-wide and per-provider
/// [`Spending`](crate::agents::spending::Spending), publishing the stamped
/// result to the shared `provider-spending.json` — the cache consumer tabs read
/// instead of walking, and the producer's own gate: a stamp younger than
/// [`SPENDING_TTL`](crate::agents::spending::SPENDING_TTL) serves the published
/// totals with zero transcript IO — no adapter discovery, no shared
/// `spending.json` cursor read, no price-book load.
///
/// The stale walk is single-flighted across every workspace for this user. The
/// elected producer reads the shared `spending.json` cache, refreshes only files
/// whose mtime changed, writes it back if anything was updated, and loads the
/// shared price book (a TTL-gated remote refresh) so Codex's token counts become
/// dollars. A timeout fallback computes an uncached in-memory result for its own
/// frame and leaves the shared files to the elected producer.
///
/// Every registered adapter is discovered fleet-wide
/// ([`transcript_files`](crate::agents::AgentAdapter::transcript_files)) so each
/// counts on the same footing, and the dashboard panel and fleet ledger read
/// one provider's spend the same way regardless of which project it ran in.
pub(super) fn compute_fleet_spending(
    runtime: &crate::RuntimePaths,
) -> crate::agents::spending::ProviderSpendingCache {
    use crate::agents::spending::read_provider_spending_cache;

    let now_ms = unix_now_ms();
    let provider_path = runtime.shared_provider_spending_path();
    // Fresh stamp: the published walk is young enough — serve it back with the
    // same single small read a consumer tab pays.
    let published = read_provider_spending_cache(&provider_path);
    if published.is_fresh(now_ms) {
        return published;
    }

    let fresh = || {
        let cache = read_provider_spending_cache(&provider_path);
        cache.is_fresh(unix_now_ms()).then_some(cache)
    };
    match crate::ledger::single_flight::coalesce(
        &runtime.shared_spending_lock(),
        SPENDING_WAIT_STEP,
        SPENDING_WAIT_STEPS,
        fresh,
    ) {
        crate::ledger::single_flight::Coalesced::Shared(cache) => cache,
        crate::ledger::single_flight::Coalesced::Produce(_guard) => {
            walk_fleet_spending(runtime, true)
        }
        crate::ledger::single_flight::Coalesced::ProduceLocal => {
            walk_fleet_spending(runtime, false)
        }
    }
}

fn walk_fleet_spending(
    runtime: &crate::RuntimePaths,
    publish: bool,
) -> crate::agents::spending::ProviderSpendingCache {
    use crate::agents::pricing;
    use crate::agents::spending::{
        PROVIDER_SPENDING_VERSION, ProviderSpendingCache, Spending, compute_spending,
        read_spending_cache, unix_secs_now, write_provider_spending_cache, write_spending_cache,
    };
    use crate::agents::{ADAPTERS, AgentAdapter};

    let now_ms = unix_now_ms();
    let provider_path = runtime.shared_provider_spending_path();
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
        let spending = Spending::default();
        if publish {
            // Stamp the empty result too: an agentless machine must not re-run
            // the (empty) discovery readdirs every tick.
            write_provider_spending_cache(&provider_path, now_ms, &spending);
        }
        return ProviderSpendingCache {
            version: PROVIDER_SPENDING_VERSION,
            refreshed_at_ms: now_ms,
            spending,
        };
    }

    let cache_path = runtime.shared_spending_cursor_path();
    let mut cache = if publish {
        read_spending_cache(&cache_path)
    } else {
        Default::default()
    };
    // The price book exists only to price the walk, so its load (and TTL-gated
    // remote refresh) rides the stale arm with it. A local fallback uses the
    // embedded table so it never writes the shared pricing cache without the
    // spending lock.
    let prices = if publish {
        pricing::load_for_spending(&runtime.shared_pricing_cache_path())
    } else {
        pricing::PriceBook::embedded()
    };
    let spending = compute_spending(&files, &mut cache, &prices, unix_secs_now());
    if publish && cache.dirty {
        write_spending_cache(&cache_path, &cache);
    }
    if publish {
        write_provider_spending_cache(&provider_path, now_ms, &spending);
    }
    ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: now_ms,
        spending,
    }
}

#[cfg(test)]
mod tests {
    use crate::RuntimePaths;
    use crate::agents::spending::{Spending, write_provider_spending_cache};
    use crate::ids::WorkspaceId;
    use crate::ledger::single_flight::{Coalesced, coalesce};
    use crate::sidebar::cache::unix_now_ms;

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::compute_fleet_spending;

    #[test]
    fn fresh_shared_publish_returns_without_walking() {
        let dir = tempfile::tempdir().unwrap();
        let first = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        let second = RuntimePaths::under(
            WorkspaceId::from_project_root(&dir.path().join("other")),
            dir.path(),
        )
        .expect("runtime paths");
        first.ensure_dirs().expect("runtime dirs");
        second.ensure_dirs().expect("runtime dirs");
        let published_at = unix_now_ms();
        let mut spending = Spending::default();
        spending.total.today.usd = 1.23;
        write_provider_spending_cache(
            &first.shared_provider_spending_path(),
            published_at,
            &spending,
        );

        let cache = compute_fleet_spending(&second);

        assert_eq!(cache.refreshed_at_ms, published_at);
        assert_eq!(cache.spending, spending);
    }

    #[test]
    fn shared_spending_lock_serves_the_elected_publish_to_a_contender() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");

        let guard = match coalesce::<crate::agents::spending::ProviderSpendingCache>(
            &runtime.shared_spending_lock(),
            Duration::ZERO,
            1,
            || None,
        ) {
            Coalesced::Produce(guard) => guard,
            _ => panic!("first contender must hold the shared spending election"),
        };

        let published_at = unix_now_ms();
        let mut spending = Spending::default();
        spending.total.today.usd = 4.56;
        let polls = AtomicU32::new(0);
        let outcome = coalesce(&runtime.shared_spending_lock(), Duration::ZERO, 3, || {
            if polls.fetch_add(1, Ordering::SeqCst) == 1 {
                write_provider_spending_cache(
                    &runtime.shared_provider_spending_path(),
                    published_at,
                    &spending,
                );
            }
            let cache = crate::agents::spending::read_provider_spending_cache(
                &runtime.shared_provider_spending_path(),
            );
            cache.is_fresh(unix_now_ms()).then_some(cache)
        });

        drop(guard);
        let Coalesced::Shared(cache) = outcome else {
            panic!("a contender must consume the elected producer's publish");
        };
        assert_eq!(cache.refreshed_at_ms, published_at);
        assert_eq!(cache.spending, spending);
    }
}
