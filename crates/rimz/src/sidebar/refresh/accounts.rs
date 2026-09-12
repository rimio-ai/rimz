use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::account::AccountProbe;
use crate::agents::{AgentAccount, ProviderLogin, RoomLoginSet};
use crate::ids::LoginKey;
use crate::sidebar::timing::{ACCOUNTS_RETRY_TTL, ACCOUNTS_TTL};
use crate::utils::time::unix_now_ms;

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
    key: LoginKey,
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

/// The producer's published account probe state, keyed by login so one
/// transient failure retries without expiring every account's successful read.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountsCache {
    pub logins: BTreeMap<LoginKey, ProviderRecord>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    LoggedIn,
    LoggedOut,
    Unavailable,
}

impl ProviderStatus {
    pub fn from_record(record: Option<&ProviderRecord>) -> Self {
        match record {
            Some(record) if record.ok && record.account.is_some() => Self::LoggedIn,
            Some(record) if record.ok => Self::LoggedOut,
            Some(_) | None => Self::Unavailable,
        }
    }
}

/// Resolve provider accounts for the producer behind a process-wide
/// single-flight. Fresh logins ride their own timestamps; only due logins
/// fork, and the winner merges those records into the shared cache.
pub(super) fn produce_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
) -> BTreeMap<String, AgentAccount> {
    let selected = provider_logins(snapshot, logins);
    produce_accounts_with(
        snapshot,
        runtime,
        logins,
        |snapshot, runtime, due, cache| probe_accounts(snapshot, runtime, &selected, due, cache),
    )
}

fn produce_accounts_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
    probe: impl Fn(
        &SidebarSnapshot,
        &RuntimePaths,
        &BTreeSet<LoginKey>,
        &AccountsCache,
    ) -> AccountsCache,
) -> BTreeMap<String, AgentAccount> {
    let context_versions = context_versions(snapshot);
    let cache = query_provider_accounts_with(
        snapshot,
        runtime,
        &provider_logins(snapshot, logins),
        false,
        probe,
    );
    accounts_with_context_versions(&cache, &context_versions, logins)
}

/// Query the requested logins through the shared account-cache single-flight.
/// A forced query bypasses per-login TTLs for this call while
/// preserving cache publication and contention behavior.
pub fn query_provider_accounts(
    runtime: &RuntimePaths,
    logins: &[ProviderLogin],
    force: bool,
) -> AccountsCache {
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    query_provider_accounts_with(
        &snapshot,
        runtime,
        logins,
        force,
        |snapshot, runtime, due, cache| probe_accounts(snapshot, runtime, logins, due, cache),
    )
}

fn query_provider_accounts_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &[ProviderLogin],
    force: bool,
    probe: impl Fn(
        &SidebarSnapshot,
        &RuntimePaths,
        &BTreeSet<LoginKey>,
        &AccountsCache,
    ) -> AccountsCache,
) -> AccountsCache {
    let path = runtime.shared_accounts_path();
    let cache = read_accounts_cache(&path);
    if !force && due_provider_logins(&cache, snapshot, logins, unix_now_ms()).is_empty() {
        return cache;
    }

    let lock_path = runtime.shared_accounts_lock();
    let fresh = || {
        if force {
            return None;
        }
        let cache = read_accounts_cache(&path);
        due_provider_logins(&cache, snapshot, logins, unix_now_ms())
            .is_empty()
            .then_some(cache)
    };
    let coordination_started = Instant::now();
    match crate::disk::single_flight::coordinate(
        &lock_path,
        ACCOUNTS_WAIT_STEP,
        ACCOUNTS_WAIT_STEPS,
        fresh,
    ) {
        crate::disk::single_flight::Coordination::Shared(cache) => cache,
        crate::disk::single_flight::Coordination::Produce(_guard) => {
            let cache = read_accounts_cache(&path);
            let due = if force {
                logins.iter().map(ProviderLogin::key).collect()
            } else {
                due_provider_logins(&cache, snapshot, logins, unix_now_ms())
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
        crate::disk::single_flight::Coordination::Unavailable => {
            let cache = read_accounts_cache(&path);
            let due = if force {
                logins.iter().map(ProviderLogin::key).collect()
            } else {
                due_provider_logins(&cache, snapshot, logins, unix_now_ms())
            };
            probe(snapshot, runtime, &due, &cache)
        }
        // A live producer still owns publication. Serve current cache truth and
        // let the next tick observe its atomic write instead of duplicating the
        // cold subprocess batch.
        crate::disk::single_flight::Coordination::ContentionTimeout => {
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

pub(in crate::sidebar) fn cached_accounts_for_snapshot(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    logins: &RoomLoginSet,
) -> BTreeMap<String, AgentAccount> {
    let cache = read_accounts_cache(&runtime.shared_accounts_path());
    accounts_with_context_versions(&cache, &context_versions(snapshot), logins)
}

/// Cheap scheduling hint from the already-published account cache.
pub(super) fn cached_account_usage_hint(
    runtime: &RuntimePaths,
    key: &LoginKey,
) -> Option<crate::agents::AccountUsageIdentity> {
    let cache = read_accounts_cache(&runtime.shared_accounts_path());
    let account = cache.logins.get(key)?.account.as_ref()?;
    Some(crate::agents::AccountUsageIdentity {
        scope: account.scope.clone(),
        credentials_stamp: account.credentials_updated_at_ms,
        account_key: None,
    })
}

fn due_provider_logins(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
    logins: &[ProviderLogin],
    now_ms: u64,
) -> BTreeSet<LoginKey> {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    let context_versions = context_versions(snapshot);
    logins
        .iter()
        .map(ProviderLogin::key)
        .filter(|key| {
            let kind = key.kind.as_str();
            let Some(record) = cache.logins.get(key) else {
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
                && !context_versions.contains_key(kind)
                && account_version(record.account.as_ref()).is_none()
        })
        .collect()
}

fn provider_logins(snapshot: &SidebarSnapshot, logins: &RoomLoginSet) -> Vec<ProviderLogin> {
    provider_kinds(snapshot)
        .iter()
        .filter_map(|kind| logins.login(kind))
        .collect()
}

fn provider_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let mut kinds: BTreeSet<String> = crate::agents::known_kinds().map(str::to_owned).collect();
    kinds.extend(active_version_probe_kinds(snapshot));
    kinds
}

/// Keep the adapter calls at the edge; `probe_accounts_with` owns the pure
/// per-login record merge and is exercised without subprocesses in unit tests.
fn probe_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &[ProviderLogin],
    due_keys: &BTreeSet<LoginKey>,
    previous: &AccountsCache,
) -> AccountsCache {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    let probed_at_ms = unix_now_ms();
    let due: Vec<_> = logins
        .iter()
        .filter(|login| due_keys.contains(&login.key()))
        .cloned()
        .collect();
    let batch = execute_account_probes(&due, &active_version_kinds, probe_one_account);
    let success_count = batch
        .results
        .iter()
        .filter(|result| result.outcome_class != ProbeOutcomeClass::Unavailable)
        .count();
    let unavailable_count = batch.results.len().saturating_sub(success_count);
    for result in &batch.results {
        if result.outcome_class == ProbeOutcomeClass::Unavailable {
            tracing::warn!(
                kind = result.key.kind.as_str(),
                tags.operation = "accounts.probe_unavailable",
                "provider account probe unavailable",
            );
        }
        trace::record(runtime, || TraceEvent::ProviderProbe {
            kind: result.key.kind.as_str(),
            outcome: result.outcome_class.as_str(),
            account_ms: result.account_ms,
            version_ms: result.version_ms,
            total_ms: result.total_ms,
        });
    }
    trace::record(runtime, || TraceEvent::ProbeBatch {
        due_count: due_keys.len(),
        worker_count: batch.worker_count,
        total_ms: batch.total_ms,
        success_count,
        unavailable_count,
    });
    merge_probe_results(previous, &active_version_kinds, probed_at_ms, batch.results)
}

fn probe_one_account(login: &ProviderLogin, active: bool) -> Option<ProviderProbeResult> {
    let login_env = login.env(&crate::agents::ambient_env());
    let kind = login.kind().as_str();
    let started = Instant::now();
    let adapter = crate::agents::find_definition(kind)?;
    let account_started = Instant::now();
    let outcome = adapter.probe_account(&login_env);
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
        key: login.key(),
        outcome,
        outcome_class,
        version,
        account_ms,
        version_ms,
        total_ms: duration_ms(started.elapsed()),
    })
}

fn execute_account_probes(
    logins: &[ProviderLogin],
    active_version_kinds: &BTreeSet<String>,
    probe: impl Fn(&ProviderLogin, bool) -> Option<ProviderProbeResult> + Sync,
) -> ProbeBatch {
    if logins.is_empty() {
        return ProbeBatch {
            results: Vec::new(),
            worker_count: 0,
            total_ms: 0,
        };
    }
    let started = Instant::now();
    let jobs: Vec<_> = logins.to_vec();
    let worker_count = MAX_PARALLEL_ACCOUNT_PROBES.min(jobs.len());
    let results: Vec<_> =
        super::runner::bounded_map(crate::lane::current(), worker_count, &jobs, |login| {
            probe(login, active_version_kinds.contains(login.kind().as_str()))
        })
        .into_iter()
        .flatten()
        .collect();
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
    let mut logins = previous.logins.clone();
    for result in results {
        let ProviderProbeResult {
            key,
            outcome,
            version: probed_version,
            ..
        } = result;
        let active = active_version_kinds.contains(key.kind.as_str());
        let ok = !matches!(&outcome, AccountProbe::Unavailable);
        let previous_record = previous.logins.get(&key);
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
        logins.insert(
            key,
            ProviderRecord {
                probed_at_ms,
                ok,
                account,
            },
        );
    }
    AccountsCache { logins }
}

#[cfg(test)]
fn probe_accounts_with(
    due_keys: &BTreeSet<LoginKey>,
    previous: &AccountsCache,
    active_version_kinds: &BTreeSet<String>,
    probed_at_ms: u64,
    mut probe: impl FnMut(&str, bool) -> Option<(AccountProbe, Option<String>)>,
) -> AccountsCache {
    let results = due_keys.iter().filter_map(|key| {
        let kind = key.kind.as_str();
        let active = active_version_kinds.contains(kind);
        let (outcome, version) = probe(kind, active)?;
        let outcome_class = match &outcome {
            AccountProbe::Found(_) => ProbeOutcomeClass::Success,
            AccountProbe::LoggedOut => ProbeOutcomeClass::LoggedOut,
            AccountProbe::Unavailable => ProbeOutcomeClass::Unavailable,
        };
        Some(ProviderProbeResult {
            key: key.clone(),
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
    logins: &RoomLoginSet,
) -> BTreeMap<String, AgentAccount> {
    let accounts = cache
        .logins
        .iter()
        .filter(|(key, _)| logins.key(key.kind.as_str()).as_ref() == Some(*key))
        .filter_map(|(key, record)| {
            record
                .account
                .as_ref()
                .map(|account| (key.kind.to_string(), account.clone()))
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
        if agent.is_provider_subagent() {
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
        .filter(|agent| !agent.is_provider_subagent())
        .filter_map(|agent| {
            crate::agents::find_definition(agent.kind.as_str()).map(|_| agent.kind.to_string())
        })
        .collect()
}

/// Read the producer's published account cache, or an empty cache on a cold,
/// corrupt, or old-schema file. Read-only and fork-free.
fn read_accounts_cache(path: &Path) -> AccountsCache {
    crate::disk::atomic::read_json_cache(path)
}

/// Publish the probed account cache atomically so readers never observe a
/// partially merged provider map. A write failure leaves the prior cache live.
pub(super) fn write_accounts_cache(path: &Path, cache: &AccountsCache) {
    if let Err(err) = crate::disk::atomic::write_temp_then_rename_cache(path, cache) {
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};

    use jiff::Timestamp;

    use super::*;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::root_agent;
    use crate::store::snapshot::SidebarSnapshot;

    fn key(kind: &str) -> LoginKey {
        LoginKey::default_for(crate::ids::AgentKind::new_unchecked(kind))
    }

    fn native_logins() -> Vec<ProviderLogin> {
        provider_logins(&empty_snapshot(), &RoomLoginSet::native())
    }

    fn record(probed_at_ms: u64, ok: bool, account: Option<AgentAccount>) -> ProviderRecord {
        ProviderRecord {
            probed_at_ms,
            ok,
            account,
        }
    }

    fn fresh_cache(now_ms: u64) -> AccountsCache {
        AccountsCache {
            logins: crate::agents::known_kinds()
                .map(|kind| (key(kind), record(now_ms, true, None)))
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
            key: key(kind),
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
        let due: Vec<_> = (0..8)
            .map(|index| {
                ProviderLogin::default_for(crate::ids::AgentKind::new_unchecked(format!(
                    "kind-{index}"
                )))
            })
            .collect();
        let barrier = Arc::new(Barrier::new(MAX_PARALLEL_ACCOUNT_PROBES));
        let first_wave = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let calls = Mutex::new(BTreeMap::<String, usize>::new());

        let batch = execute_account_probes(&due, &BTreeSet::new(), |login, _active| {
            let kind = login.kind().as_str();
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
            due.into_iter()
                .map(|login| (login.kind().to_string(), 1))
                .collect()
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
        let due: Vec<_> = ["ok", "panic"]
            .into_iter()
            .map(|kind| ProviderLogin::default_for(key(kind).kind))
            .collect();
        let prior = record(
            7,
            true,
            Some(AgentAccount {
                plan: Some("prior".to_owned()),
                ..Default::default()
            }),
        );
        let previous = AccountsCache {
            logins: BTreeMap::from([(key("panic"), prior.clone())]),
        };
        let batch = execute_account_probes(&due, &BTreeSet::new(), |login, _active| {
            let kind = login.kind().as_str();
            assert_ne!(kind, "panic", "injected worker failure");
            Some(successful_probe(kind))
        });
        let merged = merge_probe_results(&previous, &BTreeSet::new(), 20, batch.results);

        assert_eq!(merged.logins[&key("panic")], prior);
        assert_eq!(merged.logins[&key("ok")].probed_at_ms, 20);
    }

    #[test]
    fn contending_account_caller_serves_cache_without_adapter_probes() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let _producer = match crate::disk::single_flight::coordinate::<()>(
            &runtime.shared_accounts_lock(),
            Duration::ZERO,
            0,
            || None,
        ) {
            crate::disk::single_flight::Coordination::Produce(guard) => guard,
            _ => panic!("test must hold the account producer lock"),
        };
        let probes = AtomicUsize::new(0);

        assert!(
            produce_accounts_with(
                &empty_snapshot(),
                &runtime,
                &RoomLoginSet::native(),
                |_, _, _, cache| {
                    probes.fetch_add(1, Ordering::SeqCst);
                    cache.clone()
                }
            )
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

        let refreshed = query_provider_accounts_with(
            &snapshot,
            &runtime,
            &native_logins(),
            true,
            |_, _, kinds, cache| {
                due.lock().unwrap().push(kinds.clone());
                cache.clone()
            },
        );

        assert_eq!(
            due.into_inner().unwrap(),
            [native_logins().iter().map(ProviderLogin::key).collect()]
        );
        assert_eq!(refreshed.logins.len(), crate::agents::known_kinds().count());
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

        let cached = query_provider_accounts_with(
            &empty_snapshot(),
            &runtime,
            &native_logins(),
            false,
            |_, _, _, cache| {
                probes.fetch_add(1, Ordering::SeqCst);
                cache.clone()
            },
        );

        assert_eq!(cached, expected);
        assert_eq!(probes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_provider_retries_alone_without_refreshing_successful_records() {
        let now_ms = unix_now_ms();
        let stale_ms = now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
        let mut cache = fresh_cache(now_ms);
        cache.logins.insert(
            key("copilot"),
            record(
                stale_ms,
                false,
                Some(AgentAccount {
                    account_id: Some("octocat".to_owned()),
                    ..Default::default()
                }),
            ),
        );
        cache.logins.insert(
            key("claude"),
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
        let due = due_provider_logins(&cache, &snapshot, &native_logins(), now_ms);
        assert_eq!(due, BTreeSet::from([key("copilot")]));

        let successful = cache.logins[&key("claude")].clone();
        let mut probed = Vec::new();
        let merged =
            probe_accounts_with(&due, &cache, &BTreeSet::new(), now_ms, |kind, _active| {
                probed.push(kind.to_owned());
                Some((AccountProbe::Unavailable, None))
            });

        assert_eq!(probed, ["copilot"]);
        assert_eq!(merged.logins[&key("claude")], successful);
        assert_eq!(merged.logins[&key("copilot")].probed_at_ms, now_ms);
    }

    #[test]
    fn unavailable_probe_keeps_last_known_account() {
        let previous_account = AgentAccount {
            plan: Some("pro".to_owned()),
            account_id: Some("octocat".to_owned()),
            ..Default::default()
        };
        let previous = AccountsCache {
            logins: BTreeMap::from([(
                key("copilot"),
                record(10, true, Some(previous_account.clone())),
            )]),
        };

        let merged = probe_accounts_with(
            &BTreeSet::from([key("copilot")]),
            &previous,
            &BTreeSet::new(),
            20,
            |_kind, _active| Some((AccountProbe::Unavailable, None)),
        );

        assert_eq!(
            merged.logins[&key("copilot")].account,
            Some(previous_account)
        );
        assert!(!merged.logins[&key("copilot")].ok);

        let active = probe_accounts_with(
            &BTreeSet::from([key("copilot")]),
            &previous,
            &BTreeSet::from(["copilot".to_owned()]),
            20,
            |_kind, _active| Some((AccountProbe::Unavailable, Some("1.0.0".to_owned()))),
        );
        let account = active.logins[&key("copilot")].account.as_ref().unwrap();
        assert_eq!(account.account_id.as_deref(), Some("octocat"));
        assert_eq!(account.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn refreshed_accounts_keep_versions_and_logged_out_active_kinds_keep_version_only_records() {
        let previous = AccountsCache {
            logins: BTreeMap::from([(
                key("pi"),
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
        let due = BTreeSet::from([key("pi")]);
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
            found.logins[&key("pi")]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            Some("0.78.0")
        );

        let logged_out = probe_accounts_with(&due, &previous, &active, 20, |_kind, _active| {
            Some((AccountProbe::LoggedOut, None))
        });
        assert_eq!(
            logged_out.logins[&key("pi")]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            Some("0.78.0")
        );

        let idle = probe_accounts_with(&due, &previous, &BTreeSet::new(), 20, |_kind, _active| {
            Some((AccountProbe::LoggedOut, None))
        });
        assert_eq!(idle.logins[&key("pi")].account, None);
    }

    #[test]
    fn live_context_versions_merge_without_writing_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        write_accounts_cache(
            &path,
            &AccountsCache {
                logins: BTreeMap::from([(
                    key("codex"),
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

        let merged = accounts_with_context_versions(&cache, &versions, &RoomLoginSet::native());
        let persisted = read_accounts_cache(&path);

        assert_eq!(persisted.logins[&key("codex")].probed_at_ms, 42);
        assert_eq!(
            merged
                .get("codex")
                .and_then(|account| account.version.as_deref()),
            Some("0.135.0")
        );
        assert_eq!(
            persisted.logins[&key("codex")]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            None,
            "context versions remain local to the frame"
        );
    }

    #[test]
    fn account_merges_and_projection_are_login_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let work_key: LoginKey = "claude@work".parse().unwrap();
        let accounts = toml::from_str("[claude.work]\nhome = \"/srv/rimz-test-work\"\n").unwrap();
        let work = RoomLoginSet::new(
            Some(crate::ids::RoomLogins::from([(
                work_key.kind.clone(),
                work_key.name.clone(),
            )])),
            Some(crate::agents::LoginCatalog::from_config(&accounts).unwrap()),
            BTreeMap::new(),
        );
        let mut cache = AccountsCache::default();
        for (login, plan) in [
            (key("claude"), "default"),
            (work_key.clone(), "work"),
            (key("claude"), "updated"),
        ] {
            let mut result = successful_probe("claude");
            result.key = login;
            result.outcome = AccountProbe::Found(AgentAccount {
                plan: Some(plan.to_owned()),
                ..Default::default()
            });
            cache = merge_probe_results(&cache, &BTreeSet::new(), 100, [result]);
        }
        assert_eq!(cache.logins.len(), 2);
        assert_eq!(
            cache.logins[&key("claude")]
                .account
                .as_ref()
                .unwrap()
                .plan
                .as_deref(),
            Some("updated")
        );
        assert_eq!(
            cache.logins[&work_key]
                .account
                .as_ref()
                .unwrap()
                .plan
                .as_deref(),
            Some("work")
        );
        write_accounts_cache(&runtime.shared_accounts_path(), &cache);
        assert_eq!(
            cached_accounts_for_snapshot(&runtime, &empty_snapshot(), &work)["claude"]
                .plan
                .as_deref(),
            Some("work")
        );
    }

    #[test]
    fn old_schema_cache_is_discarded_and_every_provider_is_due() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        std::fs::write(&path, br#"{"providers":{}}"#).unwrap();

        let cache = read_accounts_cache(&path);

        assert!(cache.logins.is_empty());
        let snapshot = empty_snapshot();
        assert_eq!(
            due_provider_logins(&cache, &snapshot, &native_logins(), 100),
            native_logins().iter().map(ProviderLogin::key).collect()
        );
    }

    #[test]
    fn missing_probeable_versions_refresh_per_provider_on_retry_cadence() {
        for kind in ["claude", "codex", "amp", "pi", "opencode", "kiro", "kimi"] {
            let snapshot = snapshot_with(kind);
            let now_ms = unix_now_ms();
            let mut cache = fresh_cache(now_ms);
            cache.logins.insert(
                key(kind),
                record(
                    now_ms,
                    true,
                    Some(AgentAccount {
                        plan: Some("Pro".to_owned()),
                        ..Default::default()
                    }),
                ),
            );
            assert!(
                !due_provider_logins(&cache, &snapshot, &native_logins(), now_ms)
                    .contains(&key(kind))
            );

            cache.logins.get_mut(&key(kind)).unwrap().probed_at_ms =
                now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
            assert!(
                due_provider_logins(&cache, &snapshot, &native_logins(), now_ms)
                    .contains(&key(kind)),
                "an active {kind} account without a version re-probes after the retry window"
            );

            cache.logins.get_mut(&key(kind)).unwrap().ok = false;
            cache.logins.get_mut(&key(kind)).unwrap().probed_at_ms = now_ms;
            assert!(
                !due_provider_logins(&cache, &snapshot, &native_logins(), now_ms)
                    .contains(&key(kind)),
                "a failed {kind} probe waits for its own failure TTL"
            );
        }
    }
}
