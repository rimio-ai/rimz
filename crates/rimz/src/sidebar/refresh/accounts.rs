use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use crate::RuntimePaths;
use crate::agents::account::{
    ACCOUNTS_TTL, AccountProbe, AccountProfileAnswer, AccountProfileState, AccountStamps,
    AccountsCache, INFORMATIONAL_PROBE_TIMEOUT, ProviderRecord, RateLimitsCache, ReadingSource,
    RecordedLogin, merge_account_reading, read_accounts_cache, read_rate_limits_cache,
};
use crate::agents::capabilities::AccountProfileProbe;
use crate::agents::credits::OAUTH_HTTP_TIMEOUT_SECS;
use crate::agents::spending::read_provider_spending_cache;
use crate::agents::version::newest_version;
use crate::agents::{AccountUsageIdentity, AgentAccount, ProviderLogin, RoomLoginSet};
use crate::ids::LoginKey;
use crate::sidebar::timing::ACCOUNTS_RETRY_TTL;
use crate::utils::time::unix_now_ms;

use super::trace::{TraceEvent, duration_ms};
use super::{SidebarSnapshot, trace};

/// Unforced contenders wait up to `STEP * STEPS` for the elected prober's
/// publication, then serve current cache truth while the prober finishes.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

/// The sidebar host's constant worker cap: the most kinds whose account queues
/// it probes at once.
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
    if let AccountProfileProbe::Answered { plan, display_name } = outcome {
        state.answer = Some(AccountProfileAnswer {
            owner: owner.to_owned(),
            basis: account.plan.clone(),
            plan,
            plan_name: display_name,
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

/// One elected batch's probe results, merged into the record under its write
/// lock. `probed_at_ms` is the batch's start, the stamp the cadence ages from.
struct ProbeRun {
    probed_at_ms: u64,
    results: Vec<ProviderProbeResult>,
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
            probe_accounts(
                snapshot,
                runtime,
                &selected,
                logins,
                due,
                cache,
                policy,
                MAX_PARALLEL_ACCOUNT_PROBES,
            )
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
    ) -> ProbeRun,
) -> BTreeMap<LoginKey, AgentAccount> {
    let cache = query_provider_accounts_with(
        snapshot,
        runtime,
        &provider_logins(snapshot, logins),
        false,
        Duration::ZERO,
        |login| adapter_inputs_stamp(logins, login),
        probe,
    );
    accounts_in_use(&cache, logins)
}

/// The published account records as they stand, without probing: what a
/// caller reads again after a usage read merged a plan into them.
pub fn read_provider_accounts(runtime: &RuntimePaths) -> AccountsCache {
    read_accounts_cache(&runtime.shared_accounts_path())
}

/// Query the requested logins through the shared account-cache single-flight.
/// A forced query bypasses per-login TTLs and waits behind a live producer,
/// then performs its own probes and profile reads. Only a producer outliving
/// the wait bound, sized to the slowest peer that can hold the single-flight,
/// makes it serve the current cache instead. Every kind gets its own worker.
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
    query_provider_accounts_with(
        &snapshot,
        runtime,
        logins,
        force,
        forced_account_wait(logins, set),
        |login| adapter_inputs_stamp(set, login),
        |snapshot, runtime, due, cache, policy| {
            probe_accounts(
                snapshot,
                runtime,
                logins,
                set,
                due,
                cache,
                policy,
                usize::MAX,
            )
        },
    )
}

/// How long a forced caller waits out a peer's account batch: long enough for
/// the slowest peer that can hold the single-flight. The sidebar host walks the
/// kind queues on at most [`MAX_PARALLEL_ACCOUNT_PROBES`] workers, so it ends
/// within an even share of every login plus the longest queue; another command
/// gives each kind a worker and ends within the longest queue alone. A kind's
/// queue is the most logins `logins` or the machine catalog holds for it, and
/// at least one for every known kind, so a short re-probe list still waits out
/// a peer probing the whole catalog.
fn forced_account_wait(logins: &[ProviderLogin], set: &RoomLoginSet) -> Duration {
    let mut queues: BTreeMap<&str, usize> = crate::agents::known_kinds()
        .map(|kind| (kind, set.declared(kind).len().max(1)))
        .collect();
    let mut listed = BTreeMap::<&str, usize>::new();
    for login in logins {
        *listed.entry(login.kind().as_str()).or_default() += 1;
    }
    for (kind, count) in listed {
        let queue = queues.entry(kind).or_default();
        *queue = (*queue).max(count);
    }
    let longest = queues.values().copied().max().unwrap_or(1);
    let units = queues
        .values()
        .sum::<usize>()
        .div_ceil(MAX_PARALLEL_ACCOUNT_PROBES)
        + longest;
    // Two budgets per login leave room for credential lookup, publication, and
    // scheduling between the bounded account, version, and profile calls.
    (INFORMATIONAL_PROBE_TIMEOUT * 2 + Duration::from_secs(OAUTH_HTTP_TIMEOUT_SECS))
        * units as u32
        * 2
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
    ) -> ProbeRun,
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
            let run = probe(snapshot, runtime, &due, &cache, policy);
            let active = active_version_probe_kinds(snapshot);
            // Other writers merge live and usage readings while the batch
            // probes, so the results merge into the record as it now stands.
            update_accounts_cache(runtime, |current| {
                *current = merge_probe_results(current, &active, run.probed_at_ms, run.results);
                true
            })
            .unwrap_or_else(|| read_accounts_cache(&path))
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
            let run = probe(snapshot, runtime, &due, &cache, policy);
            let active = active_version_probe_kinds(snapshot);
            merge_probe_results(&cache, &active, run.probed_at_ms, run.results)
        }
        // A live producer still owns publication. The short unforced wait
        // avoids duplicate cold batches; a forced caller reaches this only
        // when the producer outlives its wait bound.
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
    logins: &RoomLoginSet,
) -> BTreeMap<LoginKey, AgentAccount> {
    accounts_in_use(
        &read_accounts_cache(&runtime.shared_accounts_path()),
        logins,
    )
}

/// Fold each in-use login's live session readings into its account record:
/// plan, metering and account id from the session's account, stamped with the
/// instant its source read them (the context's observation when it carries
/// none), and the version it reports. Sessions merge oldest reading first. Each
/// pass also marks in the record's `live_credentials` the credentials other
/// than the selected one that this room's live sessions run on (a card and no
/// `ended_at`, since a stopped agent's row stays unended), leaving another
/// room's marks to expire on their own. A pass without cards marks nothing. A login without a record waits
/// for its first probe, and a session whose context states nothing changes
/// nothing.
pub(super) fn merge_live_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
) {
    let in_use = logins.keys_in_use();
    let carded: BTreeSet<&str> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter(|row| row.as_agent().is_some())
        .map(|row| row.id.as_str())
        .collect();
    let mut sessions: Vec<_> = snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .filter(|agent| in_use.contains(&agent.login_key()))
        .filter_map(|agent| Some((agent, agent.context.as_ref()?)))
        .collect();
    sessions.sort_by_key(|(_, context)| live_read_at_ms(context));
    update_accounts_cache(runtime, |cache| {
        let mut changed = false;
        let now_ms = unix_now_ms();
        for (key, record) in &mut cache.logins {
            if !in_use.contains(key) {
                continue;
            }
            let providers = sessions
                .iter()
                .filter(|(agent, _)| agent.login_key() == *key && agent.ended_at.is_none())
                .filter(|(agent, _)| carded.contains(agent.agent_id.as_str()))
                .filter_map(|(_, context)| context.sub_provider.as_deref());
            changed |= record.see_live_credentials(providers, now_ms);
        }
        for (agent, context) in &sessions {
            let Some(record) = cache.logins.get_mut(&agent.login_key()) else {
                continue;
            };
            changed |= merge_live_reading(record, context);
        }
        changed
    });
}

/// When a session's account facts were read: the instant their source stamped
/// them, or the context's observation when it carries none.
fn live_read_at_ms(context: &crate::agents::AgentContext) -> u64 {
    context
        .account
        .as_ref()
        .and_then(|account| account.read_at_ms)
        .unwrap_or_else(|| context.observed_at.as_millisecond().max(0) as u64)
}

fn merge_live_reading(record: &mut ProviderRecord, context: &crate::agents::AgentContext) -> bool {
    let has_windows = context
        .rate_limits
        .as_ref()
        .is_some_and(|limits| !limits.windows.is_empty());
    if context.account.is_none() && context.agent_version.is_none() && !has_windows {
        return false;
    }
    let mut reading = context.account.clone().unwrap_or_default();
    reading.version = context.agent_version.clone();
    // A session drawing rate-limit windows is metered. The inference only fills
    // an unknown and goes through the merge like any reading, so a logout's
    // floor and another account's id still stop it. A kind without a login
    // probe reads as logged out on every pass, yet its sessions still meter.
    if reading.metered.is_none()
        && has_windows
        && record
            .account
            .as_ref()
            .is_none_or(|account| account.metered.is_none())
    {
        reading.metered = Some(true);
    }
    let read_at_ms = live_read_at_ms(context);
    let floor_before = record.stamps.floor_at_ms;
    let replaced_id_at = record.stamps.account_id_at_ms;
    let changed = record.merge_reading(&reading, read_at_ms, ReadingSource::Live);
    if record.stamps.floor_at_ms != floor_before {
        // A live switch: the switching session's own windows describe the new
        // account, and a sidecar's first windows date from its process start.
        // A session that predates the replaced account's id also drew windows
        // on that account's watch, so its unchanged windows cannot be told from
        // the other pane's and the floor stays at the switch.
        let own_windows_from = context
            .rate_limits
            .iter()
            .flat_map(|limits| &limits.windows)
            .filter_map(|window| window.observed_at)
            .map(|observed| observed.as_millisecond().max(0) as u64)
            .min()
            .map_or(read_at_ms, |earliest| earliest.min(read_at_ms));
        let switch_floor = if replaced_id_at.is_some_and(|at| own_windows_from < at) {
            read_at_ms
        } else {
            own_windows_from
        };
        record.stamps.floor_at_ms =
            Some(floor_before.map_or(switch_floor, |floor| floor.max(switch_floor)));
    }
    changed
}

/// Merge a usage read's plan into the login's account record, stamped at the
/// instant the read started. The usage identity's owner joins the reading, so a
/// read of another account applies nothing. A delegated (sub-provider) scope's
/// plan only fills an empty plan on the credential of that scope: the record
/// holds a credential label there, a different vocabulary. A read started
/// before the identity floor applies nothing, a credential's plan included.
pub(super) fn merge_usage_plan(
    runtime: &RuntimePaths,
    key: &LoginKey,
    identity: &AccountUsageIdentity,
    plan: &str,
    read_at_ms: u64,
) {
    let plan = plan.trim();
    if plan.is_empty() {
        return;
    }
    let scope = &identity.scope;
    update_accounts_cache(runtime, |cache| {
        let Some(record) = cache.logins.get_mut(key) else {
            return false;
        };
        let reading = AgentAccount {
            plan: Some(plan.to_owned()),
            account_key: identity.account_key.clone(),
            ..Default::default()
        };
        if scope.is_kind_wide() {
            return record.merge_reading(&reading, read_at_ms, ReadingSource::Usage);
        }
        let Some(account) = record.account.as_ref() else {
            return false;
        };
        let selected = account.scope == *scope;
        if selected && account.plan.is_some() {
            return false;
        }
        let reading = AgentAccount {
            scope: scope.clone(),
            sub_provider: account.sub_provider.clone().filter(|_| selected),
            ..reading
        };
        record.merge_reading(&reading, read_at_ms, ReadingSource::Usage)
    });
}

/// Read, change and publish `accounts.json` under its write lock, so each
/// writer merges into the record as the last writer left it. Writes only when
/// `apply` reports a change. `None` when the lock cannot be taken.
fn update_accounts_cache(
    runtime: &RuntimePaths,
    apply: impl FnOnce(&mut AccountsCache) -> bool,
) -> Option<AccountsCache> {
    let lock = runtime.shared_accounts_write_lock();
    let _guard = match crate::disk::lock::WorkspaceLock::acquire(&lock) {
        Ok(guard) => guard,
        Err(err) => {
            tracing::warn!(
                path = %lock.display(),
                tags.operation = "cache.accounts_write_lock",
                error = &err as &dyn std::error::Error,
                "sidebar accounts cache lock failed",
            );
            return None;
        }
    };
    let path = runtime.shared_accounts_path();
    let mut cache = read_accounts_cache(&path);
    if apply(&mut cache) {
        write_accounts_cache(&path, &cache);
    }
    Some(cache)
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

fn account_failure_ttl(consecutive_failures: u32) -> Duration {
    ACCOUNTS_RETRY_TTL
        .saturating_mul(2_u32.saturating_pow(consecutive_failures.saturating_sub(1)))
        .min(ACCOUNTS_TTL)
}

/// `inputs_stamps` holds the current stamps, read once per query: a record
/// whose stored stamp differs is due behind the retry floor, even after failure.
fn due_provider_logins(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
    logins: &[ProviderLogin],
    inputs_stamps: &BTreeMap<LoginKey, u64>,
    now_ms: u64,
) -> BTreeSet<LoginKey> {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    logins
        .iter()
        .map(ProviderLogin::key)
        .filter(|key| {
            let kind = key.kind.as_str();
            let Some(record) = cache.logins.get(key) else {
                return true;
            };
            let age_ms = now_ms.saturating_sub(record.probed_at_ms);
            let inputs_moved = record
                .inputs_stamp
                .zip(inputs_stamps.get(key))
                .is_some_and(|(stored, current)| stored != *current);
            if !record.ok {
                return age_ms
                    > account_failure_ttl(record.consecutive_failures).as_millis() as u64
                    || (inputs_moved && age_ms > ACCOUNTS_RETRY_TTL.as_millis() as u64);
            }
            if age_ms > ACCOUNTS_TTL.as_millis() as u64 {
                return true;
            }
            let provisional = record
                .account
                .as_ref()
                .is_some_and(|account| account.provisional);
            let version_missing = active_version_kinds.contains(kind)
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
#[expect(
    clippy::too_many_arguments,
    reason = "each caller passes its own login set, due keys, policy, and worker cap"
)]
fn probe_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &[ProviderLogin],
    set: &RoomLoginSet,
    due_keys: &BTreeSet<LoginKey>,
    previous: &AccountsCache,
    policy: ProfileReadPolicy,
    max_workers: usize,
) -> ProbeRun {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    let probed_at_ms = unix_now_ms();
    let session_start_ms = read_provider_spending_cache(&runtime.shared_provider_spending_path())
        .session_start_secs
        .map_or(0, |seconds| seconds.saturating_mul(1_000));
    let rate_limits = read_rate_limits_cache(&RateLimitsCache::shared_path(runtime));
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
    let batch =
        execute_account_probes(&due, &active_version_kinds, max_workers, |login, active| {
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
    ProbeRun {
        probed_at_ms,
        results: batch.results,
    }
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
    max_workers: usize,
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
    let kinds = logins
        .iter()
        .map(ProviderLogin::kind)
        .collect::<BTreeSet<_>>()
        .len();
    let worker_count = max_workers.min(kinds);
    let results: Vec<_> = map_by_kind(worker_count, logins, |login| {
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

/// Map every login on at most `max_workers` workers, one kind per worker, each
/// walking its kind's logins in list order: a provider sees one request at a
/// time while kinds run in parallel. Results come back in list order, each
/// stored as its login finishes. A login whose map panics loses its own result
/// and those of its kind's logins after it, which never start; every result
/// finished before it lands.
pub(super) fn map_by_kind<R: Send>(
    max_workers: usize,
    logins: &[ProviderLogin],
    map: impl Fn(&ProviderLogin) -> R + Sync,
) -> Vec<R> {
    let mut queues: Vec<Vec<(usize, &ProviderLogin)>> = Vec::new();
    for (index, login) in logins.iter().enumerate() {
        match queues
            .iter_mut()
            .find(|queue| queue[0].1.kind() == login.kind())
        {
            Some(queue) => queue.push((index, login)),
            None => queues.push(vec![(index, login)]),
        }
    }
    let results = std::sync::Mutex::new((0..logins.len()).map(|_| None).collect::<Vec<_>>());
    crate::utils::parallel::bounded_map(crate::lane::current(), max_workers, &queues, |queue| {
        for (index, login) in queue {
            let result = map(login);
            results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[*index] = Some(result);
        }
    });
    results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect()
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
        let previous_version = previous
            .logins
            .get(&key)
            .and_then(|record| account_version(record.account.as_ref()));
        let ok = !matches!(&outcome, AccountProbe::Unavailable(_));
        let probe_error = match &outcome {
            AccountProbe::Unavailable(cause) => Some(*cause),
            _ => None,
        };
        let previous_record = previous.logins.get(&key);
        let (login, login_at_ms) = match &outcome {
            AccountProbe::Found(_) => (Some(RecordedLogin::LoggedIn), Some(probed_at_ms)),
            AccountProbe::LoggedOut => (Some(RecordedLogin::LoggedOut), Some(probed_at_ms)),
            AccountProbe::Unavailable(_) => previous_record
                .and_then(ProviderRecord::known_login)
                .map_or((None, None), |known| (Some(known.login), Some(known.at_ms))),
        };
        let consecutive_failures = if ok {
            0
        } else {
            previous_record
                .map_or(0, |record| record.consecutive_failures)
                .saturating_add(1)
        };
        let profile = profile.or_else(|| previous_record.and_then(|record| record.profile.clone()));
        let version_probed_at_ms = version_probed_at_ms
            .or_else(|| previous_record.and_then(|record| record.version_probed_at_ms));
        // A logout clears every shared fact, so the next login starts fresh,
        // and a reading from before it describes the logged-out identity. A
        // login that reads logged out again keeps the first logout's floor,
        // and a first logout never lowers an earlier switch's floor.
        let repeated_logout =
            previous_record.is_some_and(|record| record.login == Some(RecordedLogin::LoggedOut));
        let mut stamps = match &outcome {
            AccountProbe::LoggedOut => AccountStamps {
                floor_at_ms: Some(
                    match previous_record.and_then(|record| record.stamps.floor_at_ms) {
                        Some(floor) if repeated_logout => floor,
                        Some(floor) => floor.max(probed_at_ms),
                        None => probed_at_ms,
                    },
                ),
                ..AccountStamps::default()
            },
            _ => {
                previous_record.map_or_else(AccountStamps::default, |record| record.stamps.clone())
            }
        };
        let account = match outcome {
            AccountProbe::Found(mut account) => {
                if account_version(Some(&account)).is_none() {
                    account.version = probed_version.or_else(|| {
                        previous_record.and_then(|record| account_version(record.account.as_ref()))
                    });
                }
                if let Some(answer) = profile
                    .as_ref()
                    .and_then(|state| state.answer.as_ref())
                    .filter(|answer| {
                        account.profile_fingerprint.as_deref() == Some(answer.owner.as_str())
                            && account.plan == answer.basis
                    })
                {
                    account.plan = answer.plan.clone().or(account.plan);
                    account.plan_name.clone_from(&answer.plan_name);
                }
                // A delegated credential's plan a usage read filled stays until
                // a probe states one.
                let previous_credentials = previous_record
                    .and_then(|record| record.account.as_ref())
                    .map(|account| &account.credentials);
                for (provider, credential) in &mut account.credentials {
                    let Some(previous) = previous_credentials
                        .and_then(|credentials| credentials.get(provider))
                        .filter(|previous| previous.scope == credential.scope)
                    else {
                        continue;
                    };
                    if credential.plan.is_none() {
                        credential.plan.clone_from(&previous.plan);
                        credential.plan_name.clone_from(&previous.plan_name);
                    }
                }
                // A credential file's mtime is when its facts were written;
                // a subprocess probe's facts are as new as the probe.
                let read_at_ms = account.credentials_updated_at_ms.unwrap_or(probed_at_ms);
                let mut shared = previous_record
                    .and_then(|record| record.account.clone())
                    .unwrap_or_default();
                merge_account_reading(
                    &mut shared,
                    &mut stamps,
                    &account,
                    read_at_ms,
                    ReadingSource::Probe,
                );
                Some(AgentAccount {
                    plan: shared.plan,
                    plan_name: shared.plan_name,
                    metered: shared.metered,
                    account_id: shared.account_id,
                    version: shared.version,
                    ..account
                })
            }
            AccountProbe::LoggedOut => active
                .then(|| {
                    newest_version(
                        [previous_version.as_deref(), probed_version.as_deref()]
                            .into_iter()
                            .flatten(),
                    )
                    .map(|version| AgentAccount {
                        version: Some(version.to_owned()),
                        ..Default::default()
                    })
                })
                .flatten(),
            AccountProbe::Unavailable(_) => {
                let mut account = previous_record.and_then(|record| record.account.clone());
                if let Some(version) = newest_version(
                    [previous_version.as_deref(), probed_version.as_deref()]
                        .into_iter()
                        .flatten(),
                ) {
                    account.get_or_insert_default().version = Some(version.to_owned());
                }
                account
            }
        };
        logins.insert(
            key,
            ProviderRecord {
                consecutive_failures,
                version_probed_at_ms: account_version(account.as_ref()).and(version_probed_at_ms),
                inputs_stamp,
                profile,
                probe_error,
                probed_at_ms,
                ok,
                account,
                login,
                login_at_ms,
                stamps,
                live_credentials: previous_record
                    .map(|record| record.live_credentials.clone())
                    .unwrap_or_default(),
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

/// The in-use logins' recorded accounts: what every display fold reads.
fn accounts_in_use(
    cache: &AccountsCache,
    logins: &RoomLoginSet,
) -> BTreeMap<LoginKey, AgentAccount> {
    let in_use = logins.keys_in_use();
    cache
        .logins
        .iter()
        .filter(|(key, _)| in_use.contains(*key))
        .filter_map(|(key, record)| Some((key.clone(), record.display_account()?)))
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
