use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentAccount;
use crate::sidebar::timing::{ACCOUNTS_RETRY_TTL, ACCOUNTS_TTL, unix_now_ms};

use super::SidebarSnapshot;

/// Poll cadence and budget for the accounts single-flight: a loser waits up to
/// `STEP * STEPS` for the elected prober's publish before forking its own probe.
/// Matched to the diff-stats single-flight, leaning long enough to ride the
/// elder's `claude auth status` fork rather than racing it.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

/// The producer's published provider-account map: the out-of-band login facts
/// (`claude auth status`, the `codex` auth file) the dashboard folds onto its
/// blocks. Single-flighted like the diff stats — the elder probes and publishes,
/// every other tab reads it back — so a consumer renderer forks zero subprocesses.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AccountsCache {
    /// When the producer last probed and published this map, for the TTL gate.
    pub refreshed_at_ms: u64,
    /// Probed accounts by agent kind; a logged-out provider is simply absent.
    pub accounts: BTreeMap<String, crate::agents::AgentAccount>,
    /// Whether the probe that produced this map completed without an
    /// infrastructure failure. A failed probe rides the short `ACCOUNTS_RETRY_TTL`
    /// so the producer re-forks within seconds; a successful one — including a
    /// confident logged-out — rides the long `ACCOUNTS_TTL`. Defaults to `true`
    /// so a cache written by an older build is trusted for the success window.
    #[serde(default = "accounts_probe_ok_default")]
    pub ok: bool,
}

/// The `AccountsCache::ok` default for caches written before the field existed:
/// trust them for the success window rather than forcing an immediate re-probe.
fn accounts_probe_ok_default() -> bool {
    true
}

impl AccountsCache {
    /// Whether the published map is young enough that the producer skips the
    /// re-probe this tick. A failed probe expires on the short retry TTL, a
    /// success on the long one. Saturating, so a clock that ran backwards reads
    /// fresh rather than re-probing every tick.
    pub(crate) fn is_fresh(&self, now_ms: u64) -> bool {
        let ttl = if self.ok {
            ACCOUNTS_TTL
        } else {
            ACCOUNTS_RETRY_TTL
        };
        now_ms.saturating_sub(self.refreshed_at_ms) <= ttl.as_millis() as u64
    }
}

/// Resolve the provider-account map for the producer, single-flighted behind
/// `accounts.lock` so a cold-start fleet — or several `ProduceLocal` losers when
/// the elder wedges — forks `claude auth status` once per refresh, not once per
/// tab. Fast path: a fresh `accounts.json` (under the success or failure TTL)
/// rides through with no lock and no fork. Slow path: elect one prober; losers
/// poll briefly for its publish, then fall back to an uncached local probe
/// rather than block on a wedged elder.
pub(crate) fn produce_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
) -> BTreeMap<String, crate::agents::AgentAccount> {
    let path = runtime.shared_accounts_path();
    let context_versions = context_versions(snapshot);

    // Fast path: a young publish needs no lock and no fork.
    let cache = read_accounts_cache(&path);
    let now_ms = unix_now_ms();
    if cache.is_fresh(now_ms) && !accounts_cache_version_refresh_due(&cache, snapshot, now_ms) {
        return cache_with_context_versions(cache, &context_versions).accounts;
    }

    // Slow path: elect one prober for this user's refresh window. The
    // freshness closure also serves coalesce's post-win re-check, so a peer that
    // published between our miss and the lock is honoured rather than re-forked.
    let lock_path = runtime.shared_accounts_lock();
    let fresh = || {
        let cache = read_accounts_cache(&path);
        let now_ms = unix_now_ms();
        (cache.is_fresh(now_ms) && !accounts_cache_version_refresh_due(&cache, snapshot, now_ms))
            .then(|| cache_with_context_versions(cache, &context_versions))
    };
    match crate::ledger::single_flight::coalesce(
        &lock_path,
        ACCOUNTS_WAIT_STEP,
        ACCOUNTS_WAIT_STEPS,
        fresh,
    ) {
        // A peer published a fresh map between our miss and the lock, or as we polled.
        crate::ledger::single_flight::Coalesced::Shared(cache) => cache.accounts,
        // We won: probe once and publish for every consumer and loser to read back.
        crate::ledger::single_flight::Coalesced::Produce(_guard) => {
            let (accounts, ok) = probe_accounts(snapshot);
            let active_kinds = active_provider_kinds(snapshot);
            let accounts =
                merge_versions(accounts, &cache.accounts, &active_kinds, &context_versions);
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
        crate::ledger::single_flight::Coalesced::ProduceLocal => {
            let active_kinds = active_provider_kinds(snapshot);
            merge_versions(
                probe_accounts(snapshot).0,
                &cache.accounts,
                &active_kinds,
                &context_versions,
            )
        }
    }
}

pub(crate) fn cached_accounts_for_snapshot(
    cache: AccountsCache,
    snapshot: &SidebarSnapshot,
) -> BTreeMap<String, AgentAccount> {
    cache_with_context_versions(cache, &context_versions(snapshot)).accounts
}

pub(crate) fn accounts_cache_version_refresh_due(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
    now_ms: u64,
) -> bool {
    if !cache.ok
        || now_ms.saturating_sub(cache.refreshed_at_ms) <= ACCOUNTS_RETRY_TTL.as_millis() as u64
    {
        return false;
    }
    let context_versions = context_versions(snapshot);
    active_version_probe_kinds(snapshot).iter().any(|kind| {
        context_versions
            .get(kind)
            .filter(|version| !version.is_empty())
            .is_none()
            && cache
                .accounts
                .get(kind)
                .and_then(|account| account.version.as_ref())
                .filter(|version| !version.is_empty())
                .is_none()
    })
}

/// Probe out-of-band login/account facts for every known provider plus any
/// active kind: a logged-in but idle provider still earns a dashboard block, so
/// the panel shows your accounts and budgets even between turns. A logged-out
/// provider is omitted and never appears. Returns the map alongside whether the
/// probe completed cleanly: a single `Unavailable` outcome (a binary that would
/// not run, an unreadable file) makes the whole refresh a failure so the
/// producer retries it on the short TTL. Producer-only — the probe is a
/// subprocess; consumers read the published result.
fn probe_accounts(snapshot: &SidebarSnapshot) -> (BTreeMap<String, AgentAccount>, bool) {
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
                if account.version.is_none() {
                    account.version = adapter.probe_version();
                }
                accounts.insert(kind, account);
            }
            crate::agents::account::AccountProbe::LoggedOut => {
                if active_version_kinds.contains(&kind)
                    && let Some(version) = adapter.probe_version()
                {
                    accounts.insert(
                        kind,
                        AgentAccount {
                            version: Some(version),
                            ..Default::default()
                        },
                    );
                }
            }
            crate::agents::account::AccountProbe::Unavailable => {
                ok = false;
                if active_version_kinds.contains(&kind)
                    && let Some(version) = adapter.probe_version()
                {
                    accounts.insert(
                        kind,
                        AgentAccount {
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

fn cache_with_context_versions(
    mut cache: AccountsCache,
    context_versions: &BTreeMap<String, String>,
) -> AccountsCache {
    if !context_versions.is_empty() {
        cache.accounts = merge_context_versions(cache.accounts, context_versions);
    }
    cache
}

fn merge_context_versions(
    mut accounts: BTreeMap<String, AgentAccount>,
    context_versions: &BTreeMap<String, String>,
) -> BTreeMap<String, AgentAccount> {
    for (kind, version) in context_versions {
        accounts.entry(kind.clone()).or_default().version = Some(version.clone());
    }
    accounts
}

fn merge_versions(
    mut accounts: BTreeMap<String, AgentAccount>,
    previous: &BTreeMap<String, AgentAccount>,
    active_kinds: &BTreeSet<String>,
    context_versions: &BTreeMap<String, String>,
) -> BTreeMap<String, AgentAccount> {
    for (kind, previous_account) in previous {
        let Some(version) = previous_account
            .version
            .as_ref()
            .filter(|version| !version.is_empty())
        else {
            continue;
        };
        if accounts.contains_key(kind) || active_kinds.contains(kind) {
            let account = accounts.entry(kind.clone()).or_default();
            if account.version.is_none() {
                account.version = Some(version.clone());
            }
        }
    }
    merge_context_versions(accounts, context_versions)
}

fn context_versions(snapshot: &SidebarSnapshot) -> BTreeMap<String, String> {
    let mut versions = BTreeMap::<String, (jiff::Timestamp, String)>::new();
    for agent in &snapshot.agents {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let Some(context) = agent.context.as_ref() else {
            continue;
        };
        let Some(version) = context
            .agent_version
            .as_ref()
            .filter(|version| !version.is_empty())
        else {
            continue;
        };
        let entry = versions
            .entry(agent.kind.to_string())
            .or_insert((context.observed_at, version.clone()));
        if context.observed_at > entry.0 {
            *entry = (context.observed_at, version.clone());
        }
    }
    versions
        .into_iter()
        .map(|(kind, (_observed_at, version))| (kind, version))
        .collect()
}

fn active_provider_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .map(|agent| agent.kind.to_string())
        .collect()
}

fn active_version_probe_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter_map(|agent| {
            crate::agents::find_adapter(agent.kind.as_str()).map(|_| agent.kind.to_string())
        })
        .collect()
}

/// Read the producer's published account cache, or an empty cache on a cold or
/// corrupt file. Read-only and fork-free — the consumer's view of the dashboard.
pub(crate) fn read_accounts_cache(path: &Path) -> AccountsCache {
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
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.accounts_write",
            error = &err as &dyn std::error::Error,
            "sidebar accounts cache write failed",
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use super::*;
    use crate::SidebarSnapshot;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::root_agent;
    use jiff::Timestamp;

    fn active_kinds(kinds: &[&str]) -> BTreeSet<String> {
        kinds.iter().map(|kind| (*kind).to_owned()).collect()
    }

    #[test]
    fn cached_versions_survive_missing_probe_versions_for_active_providers() {
        let mut previous = BTreeMap::new();
        previous.insert(
            "pi".to_owned(),
            AgentAccount {
                version: Some("0.78.0".to_owned()),
                ..Default::default()
            },
        );
        let mut accounts = BTreeMap::new();
        accounts.insert(
            "pi".to_owned(),
            AgentAccount {
                plan: Some("OpenAI OAuth".to_owned()),
                metered: Some(true),
                version: None,
                sub_provider: Some("openai".to_owned()),
            },
        );

        let merged = merge_versions(accounts, &previous, &active_kinds(&[]), &BTreeMap::new());

        assert_eq!(
            merged
                .get("pi")
                .and_then(|account| account.version.as_deref()),
            Some("0.78.0"),
            "a refreshed account with no version keeps the last known version"
        );

        let mut previous = BTreeMap::new();
        previous.insert(
            "pi".to_owned(),
            AgentAccount {
                version: Some("0.78.1".to_owned()),
                ..Default::default()
            },
        );

        let active = merge_versions(
            BTreeMap::new(),
            &previous,
            &active_kinds(&["pi"]),
            &BTreeMap::new(),
        );
        assert_eq!(
            active
                .get("pi")
                .and_then(|account| account.version.as_deref()),
            Some("0.78.1")
        );

        let idle = merge_versions(
            BTreeMap::new(),
            &previous,
            &active_kinds(&[]),
            &BTreeMap::new(),
        );
        assert!(
            idle.is_empty(),
            "a version-only cache entry does not create an idle provider"
        );
    }

    #[test]
    fn live_context_versions_merge_without_writing_fresh_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        let mut accounts = BTreeMap::new();
        accounts.insert(
            "codex".to_owned(),
            AgentAccount {
                metered: Some(true),
                ..Default::default()
            },
        );
        write_accounts_cache(
            &path,
            &AccountsCache {
                refreshed_at_ms: 42,
                accounts,
                ok: true,
            },
        );
        let cache = read_accounts_cache(&path);
        let versions = BTreeMap::from([("codex".to_owned(), "0.135.0".to_owned())]);

        let merged = cache_with_context_versions(cache, &versions);
        let persisted = read_accounts_cache(&path);

        assert_eq!(merged.refreshed_at_ms, 42);
        assert_eq!(persisted.refreshed_at_ms, 42);
        assert_eq!(
            merged
                .accounts
                .get("codex")
                .and_then(|account| account.version.as_deref()),
            Some("0.135.0")
        );
        assert_eq!(
            persisted
                .accounts
                .get("codex")
                .and_then(|account| account.version.as_deref()),
            None,
            "fresh-cache context merges stay local to the frame"
        );
    }

    #[test]
    fn account_cache_missing_probeable_versions_refreshes_on_retry_cadence() {
        for kind in ["claude", "codex", "pi", "opencode"] {
            let workspace = WorkspaceId::from_project_root(Path::new("/tmp/provider-version"));
            let snapshot = SidebarSnapshot::build_with_agents(
                workspace.clone(),
                Vec::new(),
                vec![root_agent(kind, "active", None)],
                Timestamp::now(),
            );
            let mut accounts = BTreeMap::new();
            accounts.insert(
                kind.to_owned(),
                crate::agents::AgentAccount {
                    plan: Some("Pro".to_owned()),
                    metered: Some(true),
                    version: None,
                    sub_provider: None,
                },
            );
            let now_ms = unix_now_ms();
            let fresh_cache = AccountsCache {
                refreshed_at_ms: now_ms,
                accounts,
                ok: true,
            };
            assert!(
                !accounts_cache_version_refresh_due(&fresh_cache, &snapshot, now_ms),
                "a just-refreshed successful {kind} cache missing a display version waits for the retry window"
            );

            let stale_cache = AccountsCache {
                refreshed_at_ms: now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1),
                ..fresh_cache
            };
            assert!(
                accounts_cache_version_refresh_due(&stale_cache, &snapshot, now_ms),
                "a successful {kind} account cache missing a display version re-probes after the retry window"
            );

            let empty_cache = AccountsCache {
                refreshed_at_ms: now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1),
                accounts: BTreeMap::new(),
                ok: true,
            };
            assert!(
                accounts_cache_version_refresh_due(&empty_cache, &snapshot, now_ms),
                "an active {kind} session can still get a version-only cache entry"
            );

            let failed_cache = AccountsCache {
                ok: false,
                ..empty_cache
            };
            assert!(
                !accounts_cache_version_refresh_due(&failed_cache, &snapshot, now_ms),
                "a failed {kind} probe uses the failure TTL, not the missing-version bypass"
            );
        }
    }
}
