use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use crate::RuntimePaths;
use crate::agents::account::{
    AccountProbe, AccountProfileAnswer, AccountProfileState, AccountsCache,
    INFORMATIONAL_PROBE_TIMEOUT, ProviderRecord, RecordedLogin, read_accounts_cache,
    read_rate_limits_cache,
};
use crate::agents::capabilities::AccountProfileProbe;
use crate::agents::credits::OAUTH_HTTP_TIMEOUT_SECS;
use crate::agents::spending::read_provider_spending_cache;
use crate::agents::version::newest_version;
use crate::agents::{AgentAccount, ProviderLogin, RoomLoginSet};
use crate::ids::LoginKey;
use crate::sidebar::timing::{ACCOUNTS_RETRY_TTL, ACCOUNTS_TTL};
use crate::utils::time::unix_now_ms;

use super::trace::{TraceEvent, duration_ms};
use super::{SidebarSnapshot, trace};

/// Unforced contenders wait up to `STEP * STEPS` for the elected prober's
/// publication, then serve current cache truth while the prober finishes.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

/// Most independent provider account/version chains probed concurrently.
const MAX_PARALLEL_ACCOUNT_PROBES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileReadPolicy {
    Disabled,
    Due,
    Forced,
}

fn profile_read_due(
    force: bool,
    now_ms: u64,
    state: Option<&AccountProfileState>,
    fingerprint: Option<&str>,
    basis: Option<&str>,
    trigger_ms: u64,
) -> bool {
    let Some(owner) = fingerprint else {
        return false;
    };
    if force {
        return true;
    }
    if state.is_some_and(|state| {
        now_ms.saturating_sub(state.attempted_at_ms) < ACCOUNTS_TTL.as_millis() as u64
    }) {
        return false;
    }
    let trigger_ms = trigger_ms.max(now_ms.saturating_sub(24 * 60 * 60 * 1_000));
    state
        .and_then(|state| state.answer.as_ref())
        .is_none_or(|answer| {
            answer.owner != owner
                || answer.basis.as_deref() != basis
                || answer.read_at_ms < trigger_ms
        })
}

/// The attempt takes the batch's `probed_at_ms`, the stamp the account cadence
/// ages from, so the floor expires exactly when the login is next account-due.
/// `now`, read before the request, stamps only the answer's read time, so a
/// trigger stamped while the request is in flight still makes the next batch due.
fn read_account_profile(
    account: &AgentAccount,
    previous: Option<&AccountProfileState>,
    policy: ProfileReadPolicy,
    trigger_ms: u64,
    probed_at_ms: u64,
    read: impl FnOnce() -> AccountProfileProbe,
    now: impl FnOnce() -> u64,
) -> Option<AccountProfileState> {
    if policy == ProfileReadPolicy::Disabled {
        return None;
    }
    let owner = account.profile_fingerprint.as_deref()?;
    if !profile_read_due(
        policy == ProfileReadPolicy::Forced,
        probed_at_ms,
        previous,
        Some(owner),
        account.plan.as_deref(),
        trigger_ms,
    ) {
        return None;
    }
    let read_at_ms = now();
    let outcome = read();
    if outcome == AccountProfileProbe::NotAttempted {
        return None;
    }
    let mut state = previous.cloned().unwrap_or_default();
    state.attempted_at_ms = probed_at_ms;
    if let AccountProfileProbe::Answered(plan) = outcome {
        state.answer = Some(AccountProfileAnswer {
            owner: owner.to_owned(),
            basis: account.plan.clone(),
            plan,
            read_at_ms,
        });
    }
    Some(state)
}

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
    /// The adapter's inputs stamp, read before the probe behind `outcome`.
    inputs_stamp: Option<u64>,
    outcome: AccountProbe,
    profile: Option<AccountProfileState>,
    outcome_class: ProbeOutcomeClass,
    version: Option<String>,
    version_probed_at_ms: Option<u64>,
    account_ms: u64,
    version_ms: u64,
    total_ms: u64,
}

struct ProbeBatch {
    results: Vec<ProviderProbeResult>,
    worker_count: usize,
    total_ms: u64,
}

/// Resolve provider accounts for the producer behind a process-wide
/// single-flight. Fresh logins ride their own timestamps; only due logins
/// fork, and the winner merges those records into the shared cache.
pub(super) fn produce_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
) -> BTreeMap<LoginKey, AgentAccount> {
    let selected = provider_logins(snapshot, logins);
    produce_accounts_with(
        snapshot,
        runtime,
        logins,
        |snapshot, runtime, due, cache, policy| {
            probe_accounts(snapshot, runtime, &selected, logins, due, cache, policy)
        },
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
        ProfileReadPolicy,
    ) -> AccountsCache,
) -> BTreeMap<LoginKey, AgentAccount> {
    let context_versions = context_versions(snapshot);
    let cache = query_provider_accounts_with(
        snapshot,
        runtime,
        &provider_logins(snapshot, logins),
        false,
        Duration::ZERO,
        |login| adapter_inputs_stamp(logins, login),
        probe,
    );
    accounts_with_context_versions(&cache, &context_versions, logins)
}

/// Query the requested logins through the shared account-cache single-flight.
/// A forced query bypasses per-login TTLs and waits behind a live producer,
/// then performs its own probes and profile reads. Only a producer outliving
/// the batch-derived wait bound makes it serve the current cache instead.
pub fn query_provider_accounts(
    runtime: &RuntimePaths,
    logins: &[ProviderLogin],
    set: &RoomLoginSet,
    force: bool,
) -> AccountsCache {
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    let waves = logins
        .len()
        .max(crate::agents::known_kinds().count())
        .div_ceil(MAX_PARALLEL_ACCOUNT_PROBES) as u32;
    // Two batch budgets leave room for credential lookup, publication, and
    // scheduling between the bounded account, version, and profile calls.
    let forced_wait = (INFORMATIONAL_PROBE_TIMEOUT * 2
        + Duration::from_secs(OAUTH_HTTP_TIMEOUT_SECS))
        * waves
        * 2;
    query_provider_accounts_with(
        &snapshot,
        runtime,
        logins,
        force,
        forced_wait,
        |login| adapter_inputs_stamp(set, login),
        |snapshot, runtime, due, cache, policy| {
            probe_accounts(snapshot, runtime, logins, set, due, cache, policy)
        },
    )
}

fn query_provider_accounts_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &[ProviderLogin],
    force: bool,
    forced_wait: Duration,
    inputs_stamp: impl Fn(&ProviderLogin) -> Option<u64>,
    probe: impl Fn(
        &SidebarSnapshot,
        &RuntimePaths,
        &BTreeSet<LoginKey>,
        &AccountsCache,
        ProfileReadPolicy,
    ) -> AccountsCache,
) -> AccountsCache {
    let path = runtime.shared_accounts_path();
    let cache = read_accounts_cache(&path);
    let stamps = if force {
        BTreeMap::new()
    } else {
        current_inputs_stamps(&cache, logins, inputs_stamp)
    };
    if !force && due_provider_logins(&cache, snapshot, logins, &stamps, unix_now_ms()).is_empty() {
        return cache;
    }

    let lock_path = runtime.shared_accounts_lock();
    let fresh = || {
        if force {
            return None;
        }
        let cache = read_accounts_cache(&path);
        due_provider_logins(&cache, snapshot, logins, &stamps, unix_now_ms())
            .is_empty()
            .then_some(cache)
    };
    let coordination_started = Instant::now();
    let coordination = loop {
        let coordination = crate::disk::single_flight::coordinate(
            &lock_path,
            ACCOUNTS_WAIT_STEP,
            if force { 0 } else { ACCOUNTS_WAIT_STEPS },
            fresh,
        );
        let contended = matches!(
            coordination,
            crate::disk::single_flight::Coordination::ContentionTimeout
        );
        if !force || !contended || coordination_started.elapsed() >= forced_wait {
            break coordination;
        }
        std::thread::sleep(ACCOUNTS_WAIT_STEP);
    };
    match coordination {
        crate::disk::single_flight::Coordination::Shared(cache) => cache,
        crate::disk::single_flight::Coordination::Produce(_guard) => {
            let cache = read_accounts_cache(&path);
            let due = if force {
                logins.iter().map(ProviderLogin::key).collect()
            } else {
                due_provider_logins(&cache, snapshot, logins, &stamps, unix_now_ms())
            };
            if due.is_empty() {
                return cache;
            }
            let policy = if force {
                ProfileReadPolicy::Forced
            } else {
                ProfileReadPolicy::Due
            };
            let cache = probe(snapshot, runtime, &due, &cache, policy);
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
                due_provider_logins(&cache, snapshot, logins, &stamps, unix_now_ms())
            };
            let policy = if force {
                ProfileReadPolicy::Forced
            } else {
                ProfileReadPolicy::Disabled
            };
            probe(snapshot, runtime, &due, &cache, policy)
        }
        // A live producer still owns publication. The short unforced wait
        // avoids duplicate cold batches; a forced caller reaches this only
        // when the producer outlives its batch-derived wait bound.
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
) -> BTreeMap<LoginKey, AgentAccount> {
    let cache = read_accounts_cache(&runtime.shared_accounts_path());
    accounts_with_context_versions(&cache, &context_versions(snapshot), logins)
}

/// Cheap scheduling hint from the already-published account cache.
pub(super) fn cached_account_usage_hint(
    runtime: &RuntimePaths,
    key: &LoginKey,
    sub_provider: Option<&str>,
) -> Option<crate::agents::AccountUsageIdentity> {
    let cache = read_accounts_cache(&runtime.shared_accounts_path());
    let account = cache.logins.get(key)?.account.as_ref()?;
    let account = match sub_provider {
        Some(provider) if account.sub_provider.as_deref() != Some(provider) => {
            account.credentials.get(provider)?
        }
        _ => account,
    };
    Some(crate::agents::AccountUsageIdentity {
        scope: account.scope.clone(),
        credentials_stamp: account.credentials_updated_at_ms,
        account_key: None,
    })
}

/// The current inputs stamp of each login whose record stored one. Only a
/// stored stamp can differ from the current one, so the other logins' adapters
/// are not asked.
fn current_inputs_stamps(
    cache: &AccountsCache,
    logins: &[ProviderLogin],
    inputs_stamp: impl Fn(&ProviderLogin) -> Option<u64>,
) -> BTreeMap<LoginKey, u64> {
    logins
        .iter()
        .filter(|login| {
            cache
                .logins
                .get(&login.key())
                .is_some_and(|record| record.inputs_stamp.is_some())
        })
        .filter_map(|login| Some((login.key(), inputs_stamp(login)?)))
        .collect()
}

fn adapter_inputs_stamp(set: &RoomLoginSet, login: &ProviderLogin) -> Option<u64> {
    crate::agents::find_definition(login.kind().as_str())?.account_inputs_stamp(&set.env(login))
}

/// `inputs_stamps` holds the current stamps, read once per query: a
/// successful record whose stored stamp differs is due behind the retry floor.
fn due_provider_logins(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
    logins: &[ProviderLogin],
    inputs_stamps: &BTreeMap<LoginKey, u64>,
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
            let inputs_moved = record
                .inputs_stamp
                .zip(inputs_stamps.get(key))
                .is_some_and(|(stored, current)| stored != *current);
            let provisional = record
                .account
                .as_ref()
                .is_some_and(|account| account.provisional);
            let version_missing = active_version_kinds.contains(kind)
                && !context_versions.contains_key(key)
                && account_version(record.account.as_ref()).is_none();
            age_ms > ACCOUNTS_RETRY_TTL.as_millis() as u64
                && (provisional || inputs_moved || version_missing)
        })
        .collect()
}

fn provider_logins(snapshot: &SidebarSnapshot, logins: &RoomLoginSet) -> Vec<ProviderLogin> {
    provider_kinds(snapshot)
        .iter()
        .flat_map(|kind| logins.in_use(kind))
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
    set: &RoomLoginSet,
    due_keys: &BTreeSet<LoginKey>,
    previous: &AccountsCache,
    policy: ProfileReadPolicy,
) -> AccountsCache {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    let probed_at_ms = unix_now_ms();
    let session_start_ms = read_provider_spending_cache(&runtime.shared_provider_spending_path())
        .session_start_secs
        .map_or(0, |seconds| seconds.saturating_mul(1_000));
    let rate_limits = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let policy = if crate::agents::credits::oauth_usage_offline() {
        ProfileReadPolicy::Disabled
    } else {
        policy
    };
    let due: Vec<_> = logins
        .iter()
        .filter(|login| due_keys.contains(&login.key()))
        .cloned()
        .collect();
    let batch = execute_account_probes(&due, &active_version_kinds, |login, active| {
        let key = login.key();
        let trigger_ms = session_start_ms.max(
            rate_limits
                .entries
                .get(&key.clone().into())
                .and_then(|entry| entry.reset_spotted_at_ms)
                .unwrap_or(0),
        );
        probe_one_account(
            login,
            set,
            active,
            previous.logins.get(&key),
            policy,
            trigger_ms,
            probed_at_ms,
        )
    });
    let success_count = batch
        .results
        .iter()
        .filter(|result| result.outcome_class != ProbeOutcomeClass::Unavailable)
        .count();
    let unavailable_count = batch.results.len().saturating_sub(success_count);
    for result in &batch.results {
        if result.outcome_class == ProbeOutcomeClass::Unavailable {
            tracing::info!(
                target: crate::observability::BREADCRUMB_TARGET,
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

fn probe_one_account(
    login: &ProviderLogin,
    set: &RoomLoginSet,
    active: bool,
    previous: Option<&ProviderRecord>,
    policy: ProfileReadPolicy,
    trigger_ms: u64,
    probed_at_ms: u64,
) -> Option<ProviderProbeResult> {
    let login_env = set.env(login);
    let kind = login.kind().as_str();
    let started = Instant::now();
    let adapter = crate::agents::find_definition(kind)?;
    let inputs_stamp = adapter.account_inputs_stamp(&login_env);
    let account_started = Instant::now();
    let account_probed_at_ms = unix_now_ms();
    let outcome = adapter.probe_account(&login_env);
    let account_ms = duration_ms(account_started.elapsed());
    let outcome_class = match &outcome {
        AccountProbe::Found(_) => ProbeOutcomeClass::Success,
        AccountProbe::LoggedOut => ProbeOutcomeClass::LoggedOut,
        AccountProbe::Unavailable(_) => ProbeOutcomeClass::Unavailable,
    };
    let version_started = Instant::now();
    let version_started_at_ms = unix_now_ms();
    let version = probe_account_version(&outcome, active, previous, version_started_at_ms, || {
        adapter.probe_version()
    });
    let version_probed_at_ms = if version.is_some() {
        Some(version_started_at_ms)
    } else if matches!(
        &outcome,
        AccountProbe::Found(account) if account_version(Some(account)).is_some()
    ) {
        Some(account_probed_at_ms)
    } else {
        None
    };
    let version_ms = duration_ms(version_started.elapsed());
    let profile = match &outcome {
        AccountProbe::Found(account) => read_account_profile(
            account,
            previous.and_then(|record| record.profile.as_ref()),
            policy,
            trigger_ms,
            probed_at_ms,
            || adapter.probe_account_profile(&login_env, account),
            unix_now_ms,
        ),
        _ => None,
    };
    Some(ProviderProbeResult {
        profile,
        key: login.key(),
        inputs_stamp,
        outcome,
        outcome_class,
        version,
        version_probed_at_ms,
        account_ms,
        version_ms,
        total_ms: duration_ms(started.elapsed()),
    })
}

fn probe_account_version(
    outcome: &AccountProbe,
    active: bool,
    previous: Option<&ProviderRecord>,
    now_ms: u64,
    probe: impl FnOnce() -> Option<String>,
) -> Option<String> {
    match outcome {
        AccountProbe::Found(account) if account_version(Some(account)).is_none() => {
            let reusable = previous.is_some_and(|record| {
                record
                    .account
                    .as_ref()
                    .is_some_and(|account| account.provisional)
                    && account_version(record.account.as_ref()).is_some()
                    && record.version_probed_at_ms.is_some_and(|read_at| {
                        now_ms.saturating_sub(read_at) < ACCOUNTS_TTL.as_millis() as u64
                    })
            });
            if reusable { None } else { probe() }
        }
        AccountProbe::LoggedOut | AccountProbe::Unavailable(_) if active => probe(),
        _ => None,
    }
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
        crate::utils::parallel::bounded_map(crate::lane::current(), worker_count, &jobs, |login| {
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
            inputs_stamp,
            outcome,
            profile,
            version: probed_version,
            version_probed_at_ms,
            ..
        } = result;
        let active = active_version_kinds.contains(key.kind.as_str());
        let ok = !matches!(&outcome, AccountProbe::Unavailable(_));
        let probe_error = match &outcome {
            AccountProbe::Unavailable(cause) => Some(*cause),
            _ => None,
        };
        let login = match &outcome {
            AccountProbe::Found(_) => Some(RecordedLogin::LoggedIn),
            AccountProbe::LoggedOut => Some(RecordedLogin::LoggedOut),
            AccountProbe::Unavailable(_) => None,
        };
        let previous_record = previous.logins.get(&key);
        let profile = profile.or_else(|| previous_record.and_then(|record| record.profile.clone()));
        let version_probed_at_ms = version_probed_at_ms
            .or_else(|| previous_record.and_then(|record| record.version_probed_at_ms));
        let account = match outcome {
            AccountProbe::Found(mut account) => {
                if account_version(Some(&account)).is_none() {
                    account.version = probed_version.or_else(|| {
                        previous_record.and_then(|record| account_version(record.account.as_ref()))
                    });
                }
                if let Some(plan) = profile
                    .as_ref()
                    .and_then(|state| state.answer.as_ref())
                    .filter(|answer| {
                        account.profile_fingerprint.as_deref() == Some(answer.owner.as_str())
                            && account.plan == answer.basis
                    })
                    .and_then(|answer| answer.plan.clone())
                {
                    account.plan = Some(plan);
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
            AccountProbe::Unavailable(_) => {
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
                version_probed_at_ms: account_version(account.as_ref()).and(version_probed_at_ms),
                inputs_stamp,
                profile,
                probe_error,
                probed_at_ms,
                ok,
                account,
                login,
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
        let version_probed_at_ms = (version.is_some()
            || matches!(
                &outcome,
                AccountProbe::Found(account) if account_version(Some(account)).is_some()
            ))
        .then_some(probed_at_ms);
        let outcome_class = match &outcome {
            AccountProbe::Found(_) => ProbeOutcomeClass::Success,
            AccountProbe::LoggedOut => ProbeOutcomeClass::LoggedOut,
            AccountProbe::Unavailable(_) => ProbeOutcomeClass::Unavailable,
        };
        Some(ProviderProbeResult {
            profile: None,
            key: key.clone(),
            inputs_stamp: None,
            outcome,
            outcome_class,
            version,
            version_probed_at_ms,
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
    context_versions: &BTreeMap<LoginKey, String>,
    logins: &RoomLoginSet,
) -> BTreeMap<LoginKey, AgentAccount> {
    let in_use = logins.keys_in_use();
    let accounts = cache
        .logins
        .iter()
        .filter(|(key, _)| in_use.contains(*key))
        .filter_map(|(key, record)| {
            record
                .account
                .as_ref()
                .map(|account| (key.clone(), account.clone()))
        })
        .collect();
    let mut accounts = merge_context_versions(accounts, context_versions);
    accounts.retain(|key, _| in_use.contains(key));
    accounts
}

fn merge_context_versions(
    mut accounts: BTreeMap<LoginKey, AgentAccount>,
    context_versions: &BTreeMap<LoginKey, String>,
) -> BTreeMap<LoginKey, AgentAccount> {
    for (kind, version) in context_versions {
        let account = accounts.entry(kind.clone()).or_default();
        account.version = newest_version(
            [version.as_str()]
                .into_iter()
                .chain(account.version.as_deref()),
        )
        .map(str::to_owned);
    }
    accounts
}

/// The newest version each login's sessions report. A login appears exactly
/// when one of its sessions reports a non-empty version.
fn context_versions(snapshot: &SidebarSnapshot) -> BTreeMap<LoginKey, String> {
    let mut reported = BTreeMap::<LoginKey, Vec<&str>>::new();
    for agent in snapshot.agents.iter() {
        if agent.is_provider_subagent() {
            continue;
        }
        let Some(version) = agent
            .context
            .as_ref()
            .and_then(|context| context.agent_version.as_deref())
        else {
            continue;
        };
        reported.entry(agent.login_key()).or_default().push(version);
    }
    reported
        .into_iter()
        .filter_map(|(key, versions)| Some((key, newest_version(versions)?.to_owned())))
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
