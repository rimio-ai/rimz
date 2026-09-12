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
mod tests;
