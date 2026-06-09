use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use crate::RuntimePaths;
use crate::sidebar::cache::{AccountsCache, unix_now_ms};

use super::SidebarSnapshot;

/// Poll cadence and budget for the accounts single-flight: a loser waits up to
/// `STEP * STEPS` for the elected prober's publish before forking its own probe.
/// Matched to the diff-stats single-flight, leaning long enough to ride the
/// elder's `claude auth status` fork rather than racing it.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

/// Resolve the provider-account map for the producer, single-flighted behind
/// `accounts.lock` so a cold-start fleet — or several `ProduceLocal` losers when
/// the elder wedges — forks `claude auth status` once per refresh, not once per
/// tab. Fast path: a fresh `accounts.json` (under the success or failure TTL)
/// rides through with no lock and no fork. Slow path: elect one prober; losers
/// poll briefly for its publish, then fall back to an uncached local probe
/// rather than block on a wedged elder.
pub(super) fn produce_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
) -> BTreeMap<String, crate::agents::AgentAccount> {
    let path = runtime.shared_accounts_path();

    // Fast path: a young publish needs no lock and no fork.
    let cache = read_accounts_cache(&path);
    if cache.is_fresh(unix_now_ms()) && !accounts_cache_missing_versions(&cache, snapshot) {
        return cache.accounts;
    }

    // Slow path: elect one prober for this user's refresh window. The
    // freshness closure also serves coalesce's post-win re-check, so a peer that
    // published between our miss and the lock is honoured rather than re-forked.
    let lock_path = runtime.shared_accounts_lock();
    let fresh = || {
        let cache = read_accounts_cache(&path);
        (cache.is_fresh(unix_now_ms()) && !accounts_cache_missing_versions(&cache, snapshot))
            .then_some(cache.accounts)
    };
    match crate::ledger::single_flight::coalesce(
        &lock_path,
        ACCOUNTS_WAIT_STEP,
        ACCOUNTS_WAIT_STEPS,
        fresh,
    ) {
        // A peer published a fresh map between our miss and the lock, or as we polled.
        crate::ledger::single_flight::Coalesced::Shared(accounts) => accounts,
        // We won: probe once and publish for every consumer and loser to read back.
        crate::ledger::single_flight::Coalesced::Produce(_guard) => {
            let (accounts, ok) = probe_accounts(snapshot);
            write_accounts_cache(
                &path,
                &AccountsCache {
                    refreshed_at_ms: unix_now_ms(),
                    accounts: accounts.clone(),
                    ok,
                },
            );
            accounts
        }
        // The elder wedged: probe locally for our own frame, but do not publish —
        // its result will be fresher, and a failed local probe must not pin the cache.
        crate::ledger::single_flight::Coalesced::ProduceLocal => probe_accounts(snapshot).0,
    }
}

/// Probe out-of-band login/account facts for every known provider plus any
/// active kind: a logged-in but idle provider still earns a dashboard block, so
/// the panel shows your accounts and budgets even between turns. A logged-out
/// provider is omitted and never appears. Returns the map alongside whether the
/// probe completed cleanly: a single `Unavailable` outcome (a binary that would
/// not run, an unreadable file) makes the whole refresh a failure so the
/// producer retries it on the short TTL. Producer-only — the probe is a
/// subprocess; consumers read the published result.
fn probe_accounts(
    snapshot: &SidebarSnapshot,
) -> (BTreeMap<String, crate::agents::AgentAccount>, bool) {
    let mut kinds: Vec<String> = crate::agents::known_kinds().map(str::to_owned).collect();
    let active_version_kinds = active_version_probe_kinds(snapshot);
    for agent in &snapshot.agents {
        if agent.parent_agent_id.is_none() && !kinds.iter().any(|known| agent.kind == **known) {
            kinds.push(agent.kind.to_string());
        }
    }
    let mut accounts: BTreeMap<String, crate::agents::AgentAccount> = BTreeMap::new();
    let mut ok = true;
    for kind in kinds {
        // An unregistered kind has no out-of-band login probe — nothing to retry.
        let Some(adapter) = crate::agents::find_adapter(&kind) else {
            continue;
        };
        match adapter.probe_account() {
            crate::agents::account::AccountProbe::Found(mut account) => {
                if adapter.probes_version() && account.version.is_none() {
                    account.version = adapter.probe_version();
                    if account.version.is_none() {
                        ok = false;
                    }
                }
                accounts.insert(kind, account);
            }
            crate::agents::account::AccountProbe::LoggedOut => {
                if active_version_kinds.contains(&kind) {
                    if let Some(version) = adapter.probe_version() {
                        accounts.insert(
                            kind,
                            crate::agents::AgentAccount {
                                version: Some(version),
                                ..Default::default()
                            },
                        );
                    } else {
                        ok = false;
                    }
                }
            }
            crate::agents::account::AccountProbe::Unavailable => {
                ok = false;
                if active_version_kinds.contains(&kind)
                    && let Some(version) = adapter.probe_version()
                {
                    accounts.insert(
                        kind,
                        crate::agents::AgentAccount {
                            version: Some(version),
                            ..Default::default()
                        },
                    );
                }
            }
        }
    }
    (accounts, ok)
}

fn active_version_probe_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter_map(|agent| {
            crate::agents::find_adapter(agent.kind.as_str())
                .filter(|adapter| adapter.probes_version())
                .map(|_| agent.kind.to_string())
        })
        .collect()
}

pub(crate) fn accounts_cache_missing_versions(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
) -> bool {
    // A failed probe already rides the short retry TTL. Honor that freshness
    // window instead of bypassing it every producer tick.
    if !cache.ok {
        return false;
    }
    if cache.accounts.iter().any(|(kind, account)| {
        account.version.is_none()
            && crate::agents::find_adapter(kind).is_some_and(|adapter| adapter.probes_version())
    }) {
        return true;
    }
    active_version_probe_kinds(snapshot)
        .into_iter()
        .any(|kind| {
            cache
                .accounts
                .get(&kind)
                .and_then(|account| account.version.as_ref())
                .is_none()
        })
}

/// Read the producer's published account cache, or an empty cache on a cold or
/// corrupt file. Read-only and fork-free — the consumer's view of the dashboard.
pub(super) fn read_accounts_cache(path: &Path) -> AccountsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Publish the probed account cache for consumer tabs to read, atomically so a
/// reader never observes a half-written file. Best-effort: a write failure logs
/// and leaves the prior cache in place.
pub(super) fn write_accounts_cache(path: &Path, cache: &AccountsCache) {
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(path = %path.display(), error = %err, "sidebar accounts cache write failed");
    }
}
