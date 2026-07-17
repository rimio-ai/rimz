use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentAccount;
use crate::agents::account::AccountProbe;
use crate::sidebar::timing::{ACCOUNTS_RETRY_TTL, ACCOUNTS_TTL, unix_now_ms};

use super::trace::{TraceEvent, duration_ms};
use super::{SidebarSnapshot, trace};

/// Poll cadence and budget for the accounts single-flight: a loser waits up to
/// `STEP * STEPS` for the elected prober's publish, then serves current cache
/// truth while the elder finishes.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

/// Most independent provider account/version chains probed concurrently.
const MAX_PARALLEL_ACCOUNT_PROBES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeOutcomeClass {
    Success,
    LoggedOut,
    Unavailable,
}

impl ProbeOutcomeClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::LoggedOut => "logged_out",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug)]
struct ProviderProbeResult {
    kind: String,
    outcome: AccountProbe,
    outcome_class: ProbeOutcomeClass,
    version: Option<String>,
    account_ms: u64,
    version_ms: u64,
    total_ms: u64,
}

struct ProbeBatch {
    results: Vec<ProviderProbeResult>,
    worker_count: usize,
    total_ms: u64,
}

/// The producer's published account probe state, keyed by provider so one
/// transient failure retries without expiring every provider's successful read.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountsCache {
    pub providers: BTreeMap<String, ProviderRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub probed_at_ms: u64,
    /// A failed probe retries on `ACCOUNTS_RETRY_TTL`; a confident result rides
    /// `ACCOUNTS_TTL`.
    pub ok: bool,
    /// Probed account facts; `None` is an authoritative logged-out result.
    pub account: Option<AgentAccount>,
}

/// Resolve provider accounts for the producer behind a process-wide
/// single-flight. Fresh providers ride their own timestamps; only due kinds
/// fork, and the winner merges those records into the shared cache.
pub(crate) fn produce_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
) -> BTreeMap<String, AgentAccount> {
    produce_accounts_with(snapshot, runtime, probe_accounts)
}

fn produce_accounts_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    probe: impl Fn(&SidebarSnapshot, &RuntimePaths, &BTreeSet<String>, &AccountsCache) -> AccountsCache,
) -> BTreeMap<String, AgentAccount> {
    let context_versions = context_versions(snapshot);
    let cache = query_provider_accounts_with(snapshot, runtime, false, probe);
    accounts_with_context_versions(&cache, &context_versions)
}

/// Query every registered provider account through the shared account-cache
/// single-flight. A forced query bypasses per-provider TTLs for this call while
/// preserving cache publication and contention behavior.
pub fn query_provider_accounts(runtime: &RuntimePaths, force: bool) -> AccountsCache {
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    query_provider_accounts_with(&snapshot, runtime, force, probe_accounts)
}

fn query_provider_accounts_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    force: bool,
    probe: impl Fn(&SidebarSnapshot, &RuntimePaths, &BTreeSet<String>, &AccountsCache) -> AccountsCache,
) -> AccountsCache {
    let path = runtime.shared_accounts_path();
    let cache = read_accounts_cache(&path);
    if !force && due_provider_kinds(&cache, snapshot, unix_now_ms()).is_empty() {
        return cache;
    }

    let lock_path = runtime.shared_accounts_lock();
    let fresh = || {
        if force {
            return None;
        }
        let cache = read_accounts_cache(&path);
        due_provider_kinds(&cache, snapshot, unix_now_ms())
            .is_empty()
            .then_some(cache)
    };
    let coordination_started = Instant::now();
    match crate::store::single_flight::coordinate(
        &lock_path,
        ACCOUNTS_WAIT_STEP,
        ACCOUNTS_WAIT_STEPS,
        fresh,
    ) {
        crate::store::single_flight::Coordination::Shared(cache) => cache,
        crate::store::single_flight::Coordination::Produce(_guard) => {
            let cache = read_accounts_cache(&path);
            let due = if force {
                provider_kinds(snapshot)
            } else {
                due_provider_kinds(&cache, snapshot, unix_now_ms())
            };
            if due.is_empty() {
                return cache;
            }
            let cache = probe(snapshot, runtime, &due, &cache);
            write_accounts_cache(&path, &cache);
            cache
        }
        // A missing coordination path cannot protect a shared publication.
        // Probe locally for this frame without writing the cache.
        crate::store::single_flight::Coordination::Unavailable => {
            let cache = read_accounts_cache(&path);
            let due = if force {
                provider_kinds(snapshot)
            } else {
                due_provider_kinds(&cache, snapshot, unix_now_ms())
            };
            probe(snapshot, runtime, &due, &cache)
        }
        // A live producer still owns publication. Serve current cache truth and
        // let the next tick observe its atomic write instead of duplicating the
        // cold subprocess batch.
        crate::store::single_flight::Coordination::ContentionTimeout => {
            let wait_ms = duration_ms(coordination_started.elapsed());
            trace::record(runtime, || TraceEvent::Contention {
                outcome: "served_stale",
                wait_ms,
            });
            tracing::debug!(
                wait_ms,
                tags.operation = "accounts.probe_contention",
                "account probe producer still running; serving current cache",
            );
            read_accounts_cache(&path)
        }
    }
}

pub(crate) fn cached_accounts_for_snapshot(
    cache: AccountsCache,
    snapshot: &SidebarSnapshot,
) -> BTreeMap<String, AgentAccount> {
    accounts_with_context_versions(&cache, &context_versions(snapshot))
}

/// Cheap scheduling hint from the already-published account cache.
pub(crate) fn cached_account_usage_hint(
    runtime: &RuntimePaths,
    kind: &str,
) -> Option<(crate::agents::ProviderAccountScope, Option<u64>)> {
    let cache = read_accounts_cache(&runtime.shared_accounts_path());
    let account = cache.providers.get(kind)?.account.as_ref()?;
    Some((account.scope.clone(), account.credentials_updated_at_ms))
}

fn due_provider_kinds(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
    now_ms: u64,
) -> BTreeSet<String> {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    let context_versions = context_versions(snapshot);
    provider_kinds(snapshot)
        .into_iter()
        .filter(|kind| {
            let Some(record) = cache.providers.get(kind) else {
                return true;
            };
            let age_ms = now_ms.saturating_sub(record.probed_at_ms);
            if !record.ok {
                return age_ms > ACCOUNTS_RETRY_TTL.as_millis() as u64;
            }
            if age_ms > ACCOUNTS_TTL.as_millis() as u64 {
                return true;
            }
            age_ms > ACCOUNTS_RETRY_TTL.as_millis() as u64
                && active_version_kinds.contains(kind)
                && context_versions
                    .get(kind)
                    .filter(|version| !version.is_empty())
                    .is_none()
                && account_version(record.account.as_ref()).is_none()
        })
        .collect()
}

fn provider_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let mut kinds: BTreeSet<String> = crate::agents::known_kinds().map(str::to_owned).collect();
    kinds.extend(
        snapshot
            .agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .filter_map(|agent| {
                crate::agents::find_adapter(agent.kind.as_str()).map(|_| agent.kind.to_string())
            }),
    );
    kinds
}

/// Keep the adapter calls at the edge; `probe_accounts_with` owns the pure
/// per-kind record merge and is exercised without subprocesses in unit tests.
fn probe_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    due_kinds: &BTreeSet<String>,
    previous: &AccountsCache,
) -> AccountsCache {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    let probed_at_ms = unix_now_ms();
    let batch = execute_account_probes(due_kinds, &active_version_kinds, probe_one_account);
    let success_count = batch
        .results
        .iter()
        .filter(|result| result.outcome_class != ProbeOutcomeClass::Unavailable)
        .count();
    let unavailable_count = batch.results.len().saturating_sub(success_count);
    for result in &batch.results {
        if result.outcome_class == ProbeOutcomeClass::Unavailable {
            tracing::warn!(
                kind = result.kind.as_str(),
                tags.operation = "accounts.probe_unavailable",
                "provider account probe unavailable",
            );
        }
        trace::record(runtime, || TraceEvent::ProviderProbe {
            kind: &result.kind,
            outcome: result.outcome_class.as_str(),
            account_ms: result.account_ms,
            version_ms: result.version_ms,
            total_ms: result.total_ms,
        });
    }
    trace::record(runtime, || TraceEvent::ProbeBatch {
        due_count: due_kinds.len(),
        worker_count: batch.worker_count,
        total_ms: batch.total_ms,
        success_count,
        unavailable_count,
    });
    merge_probe_results(previous, &active_version_kinds, probed_at_ms, batch.results)
}

fn probe_one_account(kind: &str, active: bool) -> Option<ProviderProbeResult> {
    let started = Instant::now();
    let adapter = crate::agents::find_adapter(kind)?;
    let account_started = Instant::now();
    let outcome = adapter.probe_account();
    let account_ms = duration_ms(account_started.elapsed());
    let outcome_class = match &outcome {
        AccountProbe::Found(_) => ProbeOutcomeClass::Success,
        AccountProbe::LoggedOut => ProbeOutcomeClass::LoggedOut,
        AccountProbe::Unavailable => ProbeOutcomeClass::Unavailable,
    };
    let version_started = Instant::now();
    let version = match &outcome {
        AccountProbe::Found(account) if account_version(Some(account)).is_none() => {
            adapter.probe_version()
        }
        AccountProbe::LoggedOut | AccountProbe::Unavailable if active => adapter.probe_version(),
        _ => None,
    };
    let version_ms = duration_ms(version_started.elapsed());
    Some(ProviderProbeResult {
        kind: kind.to_owned(),
        outcome,
        outcome_class,
        version,
        account_ms,
        version_ms,
        total_ms: duration_ms(started.elapsed()),
    })
}

fn execute_account_probes(
    due_kinds: &BTreeSet<String>,
    active_version_kinds: &BTreeSet<String>,
    probe: impl Fn(&str, bool) -> Option<ProviderProbeResult> + Sync,
) -> ProbeBatch {
    if due_kinds.is_empty() {
        return ProbeBatch {
            results: Vec::new(),
            worker_count: 0,
            total_ms: 0,
        };
    }
    let started = Instant::now();
    let lane = crate::lane::current();
    let jobs: Vec<_> = due_kinds.iter().cloned().collect();
    let next = AtomicUsize::new(0);
    let worker_count = MAX_PARALLEL_ACCOUNT_PROBES.min(jobs.len());
    let results = Mutex::new((0..jobs.len()).map(|_| None).collect::<Vec<_>>());
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..worker_count)
            .map(|_| {
                scope.spawn(|| {
                    crate::lane::set(lane);
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(kind) = jobs.get(index) else {
                            break;
                        };
                        let active = active_version_kinds.contains(kind);
                        let Some(result) = probe(kind, active) else {
                            continue;
                        };
                        if let Ok(mut results) = results.lock() {
                            results[index] = Some(result);
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            let _ = worker.join();
        }
    });
    let mut results: Vec<_> = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect();
    results.sort_by(|left, right| left.kind.cmp(&right.kind));
    ProbeBatch {
        results,
        worker_count,
        total_ms: duration_ms(started.elapsed()),
    }
}

fn merge_probe_results(
    previous: &AccountsCache,
    active_version_kinds: &BTreeSet<String>,
    probed_at_ms: u64,
    results: impl IntoIterator<Item = ProviderProbeResult>,
) -> AccountsCache {
    let mut providers = previous.providers.clone();
    for result in results {
        let ProviderProbeResult {
            kind,
            outcome,
            version: probed_version,
            ..
        } = result;
        let active = active_version_kinds.contains(&kind);
        let ok = !matches!(&outcome, AccountProbe::Unavailable);
        let previous_record = previous.providers.get(&kind);
        let account = match outcome {
            AccountProbe::Found(mut account) => {
                if account_version(Some(&account)).is_none() {
                    account.version = probed_version.or_else(|| {
                        previous_record.and_then(|record| account_version(record.account.as_ref()))
                    });
                }
                Some(account)
            }
            AccountProbe::LoggedOut => active
                .then(|| {
                    probed_version
                        .or_else(|| {
                            previous_record
                                .and_then(|record| account_version(record.account.as_ref()))
                        })
                        .map(|version| AgentAccount {
                            version: Some(version),
                            ..Default::default()
                        })
                })
                .flatten(),
            AccountProbe::Unavailable => {
                let mut account = previous_record.and_then(|record| record.account.clone());
                if let Some(version) = probed_version {
                    account.get_or_insert_default().version = Some(version);
                }
                account
            }
        };
        providers.insert(
            kind,
            ProviderRecord {
                probed_at_ms,
                ok,
                account,
            },
        );
    }
    AccountsCache { providers }
}

#[cfg(test)]
fn probe_accounts_with(
    due_kinds: &BTreeSet<String>,
    previous: &AccountsCache,
    active_version_kinds: &BTreeSet<String>,
    probed_at_ms: u64,
    mut probe: impl FnMut(&str, bool) -> Option<(AccountProbe, Option<String>)>,
) -> AccountsCache {
    let results = due_kinds.iter().filter_map(|kind| {
        let active = active_version_kinds.contains(kind);
        let (outcome, version) = probe(kind, active)?;
        let outcome_class = match &outcome {
            AccountProbe::Found(_) => ProbeOutcomeClass::Success,
            AccountProbe::LoggedOut => ProbeOutcomeClass::LoggedOut,
            AccountProbe::Unavailable => ProbeOutcomeClass::Unavailable,
        };
        Some(ProviderProbeResult {
            kind: kind.clone(),
            outcome,
            outcome_class,
            version,
            account_ms: 0,
            version_ms: 0,
            total_ms: 0,
        })
    });
    merge_probe_results(previous, active_version_kinds, probed_at_ms, results)
}

fn account_version(account: Option<&AgentAccount>) -> Option<String> {
    account?
        .version
        .as_ref()
        .filter(|version| !version.is_empty())
        .cloned()
}

fn accounts_with_context_versions(
    cache: &AccountsCache,
    context_versions: &BTreeMap<String, String>,
) -> BTreeMap<String, AgentAccount> {
    let accounts = cache
        .providers
        .iter()
        .filter_map(|(kind, record)| {
            record
                .account
                .as_ref()
                .map(|account| (kind.clone(), account.clone()))
        })
        .collect();
    merge_context_versions(accounts, context_versions)
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

/// Read the producer's published account cache, or an empty cache on a cold,
/// corrupt, or old-schema file. Read-only and fork-free.
pub(crate) fn read_accounts_cache(path: &Path) -> AccountsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Publish the probed account cache atomically so readers never observe a
/// partially merged provider map. A write failure leaves the prior cache live.
pub(super) fn write_accounts_cache(path: &Path, cache: &AccountsCache) {
    if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(path, cache) {
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
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Barrier, Mutex};

    use jiff::Timestamp;

    use super::*;
    use crate::SidebarSnapshot;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::root_agent;

    fn record(probed_at_ms: u64, ok: bool, account: Option<AgentAccount>) -> ProviderRecord {
        ProviderRecord {
            probed_at_ms,
            ok,
            account,
        }
    }

    fn fresh_cache(now_ms: u64) -> AccountsCache {
        AccountsCache {
            providers: crate::agents::known_kinds()
                .map(|kind| (kind.to_owned(), record(now_ms, true, None)))
                .collect(),
        }
    }

    fn snapshot_with(kind: &str) -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/provider-version")),
            vec![root_agent(kind, "active", None)],
            Timestamp::now(),
        )
    }

    fn empty_snapshot() -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/provider-accounts")),
            Vec::new(),
            Timestamp::now(),
        )
    }

    fn successful_probe(kind: &str) -> ProviderProbeResult {
        ProviderProbeResult {
            kind: kind.to_owned(),
            outcome: AccountProbe::Found(AgentAccount {
                plan: Some(kind.to_owned()),
                ..Default::default()
            }),
            outcome_class: ProbeOutcomeClass::Success,
            version: None,
            account_ms: 1,
            version_ms: 0,
            total_ms: 1,
        }
    }

    #[test]
    fn account_probe_pool_runs_each_kind_once_with_four_worker_ceiling() {
        let due: BTreeSet<_> = (0..8).map(|index| format!("kind-{index}")).collect();
        let barrier = Arc::new(Barrier::new(MAX_PARALLEL_ACCOUNT_PROBES));
        let first_wave = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let calls = Mutex::new(BTreeMap::<String, usize>::new());

        let batch = execute_account_probes(&due, &BTreeSet::new(), |kind, _active| {
            *calls.lock().unwrap().entry(kind.to_owned()).or_default() += 1;
            let live = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(live, Ordering::SeqCst);
            if first_wave.fetch_add(1, Ordering::SeqCst) < MAX_PARALLEL_ACCOUNT_PROBES {
                barrier.wait();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            Some(successful_probe(kind))
        });

        assert_eq!(batch.worker_count, MAX_PARALLEL_ACCOUNT_PROBES);
        assert_eq!(batch.results.len(), due.len());
        assert_eq!(maximum.load(Ordering::SeqCst), MAX_PARALLEL_ACCOUNT_PROBES);
        assert_eq!(
            calls.into_inner().unwrap(),
            due.into_iter().map(|kind| (kind, 1)).collect()
        );

        let merged = merge_probe_results(
            &AccountsCache::default(),
            &BTreeSet::new(),
            10,
            batch.results,
        );
        let serialized = serde_json::to_string(&merged).unwrap();
        assert!(
            serialized.find("kind-0") < serialized.find("kind-7"),
            "BTreeMap publication is deterministic regardless of worker completion order"
        );
    }

    #[test]
    fn missing_worker_result_preserves_prior_record() {
        let due = BTreeSet::from(["ok".to_owned(), "panic".to_owned()]);
        let prior = record(
            7,
            true,
            Some(AgentAccount {
                plan: Some("prior".to_owned()),
                ..Default::default()
            }),
        );
        let previous = AccountsCache {
            providers: BTreeMap::from([("panic".to_owned(), prior.clone())]),
        };
        let batch = execute_account_probes(&due, &BTreeSet::new(), |kind, _active| {
            assert_ne!(kind, "panic", "injected worker failure");
            Some(successful_probe(kind))
        });
        let merged = merge_probe_results(&previous, &BTreeSet::new(), 20, batch.results);

        assert_eq!(merged.providers["panic"], prior);
        assert_eq!(merged.providers["ok"].probed_at_ms, 20);
    }

    #[test]
    fn contending_account_caller_serves_cache_without_adapter_probes() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let _producer = match crate::store::single_flight::coordinate::<()>(
            &runtime.shared_accounts_lock(),
            Duration::ZERO,
            0,
            || None,
        ) {
            crate::store::single_flight::Coordination::Produce(guard) => guard,
            _ => panic!("test must hold the account producer lock"),
        };
        let probes = AtomicUsize::new(0);

        assert!(
            produce_accounts_with(&empty_snapshot(), &runtime, |_, _, _, cache| {
                probes.fetch_add(1, Ordering::SeqCst);
                cache.clone()
            })
            .is_empty()
        );

        assert_eq!(probes.load(Ordering::SeqCst), 0);
        assert!(!runtime.shared_accounts_path().exists());
    }

    #[test]
    fn provider_query_force_bypasses_ttl_for_every_registered_kind() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_shared_dirs().unwrap();
        write_accounts_cache(&runtime.shared_accounts_path(), &fresh_cache(unix_now_ms()));
        let snapshot = empty_snapshot();
        let due = Mutex::new(Vec::new());

        let refreshed =
            query_provider_accounts_with(&snapshot, &runtime, true, |_, _, kinds, cache| {
                due.lock().unwrap().push(kinds.clone());
                cache.clone()
            });

        assert_eq!(due.into_inner().unwrap(), [provider_kinds(&snapshot)]);
        assert_eq!(
            refreshed.providers.len(),
            crate::agents::known_kinds().count()
        );
    }

    #[test]
    fn provider_query_preserves_normal_per_provider_ttls() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_shared_dirs().unwrap();
        let expected = fresh_cache(unix_now_ms());
        write_accounts_cache(&runtime.shared_accounts_path(), &expected);
        let probes = AtomicUsize::new(0);

        let cached =
            query_provider_accounts_with(&empty_snapshot(), &runtime, false, |_, _, _, cache| {
                probes.fetch_add(1, Ordering::SeqCst);
                cache.clone()
            });

        assert_eq!(cached, expected);
        assert_eq!(probes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_provider_retries_alone_without_refreshing_successful_records() {
        let now_ms = unix_now_ms();
        let stale_ms = now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
        let mut cache = fresh_cache(now_ms);
        cache.providers.insert(
            "copilot".to_owned(),
            record(
                stale_ms,
                false,
                Some(AgentAccount {
                    account_id: Some("octocat".to_owned()),
                    ..Default::default()
                }),
            ),
        );
        cache.providers.insert(
            "claude".to_owned(),
            record(
                stale_ms,
                true,
                Some(AgentAccount {
                    plan: Some("max".to_owned()),
                    ..Default::default()
                }),
            ),
        );
        let snapshot = empty_snapshot();
        let due = due_provider_kinds(&cache, &snapshot, now_ms);
        assert_eq!(due, BTreeSet::from(["copilot".to_owned()]));

        let successful = cache.providers["claude"].clone();
        let mut probed = Vec::new();
        let merged =
            probe_accounts_with(&due, &cache, &BTreeSet::new(), now_ms, |kind, _active| {
                probed.push(kind.to_owned());
                Some((AccountProbe::Unavailable, None))
            });

        assert_eq!(probed, ["copilot"]);
        assert_eq!(merged.providers["claude"], successful);
        assert_eq!(merged.providers["copilot"].probed_at_ms, now_ms);
    }

    #[test]
    fn unavailable_probe_keeps_last_known_account() {
        let previous_account = AgentAccount {
            plan: Some("pro".to_owned()),
            account_id: Some("octocat".to_owned()),
            ..Default::default()
        };
        let previous = AccountsCache {
            providers: BTreeMap::from([(
                "copilot".to_owned(),
                record(10, true, Some(previous_account.clone())),
            )]),
        };

        let merged = probe_accounts_with(
            &BTreeSet::from(["copilot".to_owned()]),
            &previous,
            &BTreeSet::new(),
            20,
            |_kind, _active| Some((AccountProbe::Unavailable, None)),
        );

        assert_eq!(merged.providers["copilot"].account, Some(previous_account));
        assert!(!merged.providers["copilot"].ok);

        let active = probe_accounts_with(
            &BTreeSet::from(["copilot".to_owned()]),
            &previous,
            &BTreeSet::from(["copilot".to_owned()]),
            20,
            |_kind, _active| Some((AccountProbe::Unavailable, Some("1.0.0".to_owned()))),
        );
        let account = active.providers["copilot"].account.as_ref().unwrap();
        assert_eq!(account.account_id.as_deref(), Some("octocat"));
        assert_eq!(account.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn refreshed_accounts_keep_versions_and_logged_out_active_kinds_keep_version_only_records() {
        let previous = AccountsCache {
            providers: BTreeMap::from([(
                "pi".to_owned(),
                record(
                    10,
                    true,
                    Some(AgentAccount {
                        version: Some("0.78.0".to_owned()),
                        ..Default::default()
                    }),
                ),
            )]),
        };
        let due = BTreeSet::from(["pi".to_owned()]);
        let active = BTreeSet::from(["pi".to_owned()]);
        let found = probe_accounts_with(&due, &previous, &active, 20, |_kind, _active| {
            Some((
                AccountProbe::Found(AgentAccount {
                    plan: Some("OpenAI OAuth".to_owned()),
                    ..Default::default()
                }),
                None,
            ))
        });
        assert_eq!(
            found.providers["pi"]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            Some("0.78.0")
        );

        let logged_out = probe_accounts_with(&due, &previous, &active, 20, |_kind, _active| {
            Some((AccountProbe::LoggedOut, None))
        });
        assert_eq!(
            logged_out.providers["pi"]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            Some("0.78.0")
        );

        let idle = probe_accounts_with(&due, &previous, &BTreeSet::new(), 20, |_kind, _active| {
            Some((AccountProbe::LoggedOut, None))
        });
        assert_eq!(idle.providers["pi"].account, None);
    }

    #[test]
    fn live_context_versions_merge_without_writing_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        write_accounts_cache(
            &path,
            &AccountsCache {
                providers: BTreeMap::from([(
                    "codex".to_owned(),
                    record(
                        42,
                        true,
                        Some(AgentAccount {
                            metered: Some(true),
                            ..Default::default()
                        }),
                    ),
                )]),
            },
        );
        let cache = read_accounts_cache(&path);
        let versions = BTreeMap::from([("codex".to_owned(), "0.135.0".to_owned())]);

        let merged = accounts_with_context_versions(&cache, &versions);
        let persisted = read_accounts_cache(&path);

        assert_eq!(persisted.providers["codex"].probed_at_ms, 42);
        assert_eq!(
            merged
                .get("codex")
                .and_then(|account| account.version.as_deref()),
            Some("0.135.0")
        );
        assert_eq!(
            persisted.providers["codex"]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            None,
            "context versions remain local to the frame"
        );
    }

    #[test]
    fn old_schema_cache_is_discarded_and_every_provider_is_due() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        std::fs::write(&path, br#"{"refreshed_at_ms":42,"accounts":{},"ok":true}"#).unwrap();

        let cache = read_accounts_cache(&path);

        assert!(cache.providers.is_empty());
        let snapshot = empty_snapshot();
        assert_eq!(
            due_provider_kinds(&cache, &snapshot, 100),
            provider_kinds(&snapshot)
        );
    }

    #[test]
    fn missing_probeable_versions_refresh_per_provider_on_retry_cadence() {
        for kind in ["claude", "codex", "amp", "pi", "opencode", "kiro", "kimi"] {
            let snapshot = snapshot_with(kind);
            let now_ms = unix_now_ms();
            let mut cache = fresh_cache(now_ms);
            cache.providers.insert(
                kind.to_owned(),
                record(
                    now_ms,
                    true,
                    Some(AgentAccount {
                        plan: Some("Pro".to_owned()),
                        ..Default::default()
                    }),
                ),
            );
            assert!(!due_provider_kinds(&cache, &snapshot, now_ms).contains(kind));

            cache.providers.get_mut(kind).unwrap().probed_at_ms =
                now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
            assert!(
                due_provider_kinds(&cache, &snapshot, now_ms).contains(kind),
                "an active {kind} account without a version re-probes after the retry window"
            );

            cache.providers.get_mut(kind).unwrap().ok = false;
            cache.providers.get_mut(kind).unwrap().probed_at_ms = now_ms;
            assert!(
                !due_provider_kinds(&cache, &snapshot, now_ms).contains(kind),
                "a failed {kind} probe waits for its own failure TTL"
            );
        }
    }
}
