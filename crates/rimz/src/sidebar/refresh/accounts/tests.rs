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
        consecutive_failures: 0,
        version_probed_at_ms: None,
        inputs_stamp: None,
        profile: None,
        probe_error: None,
        login: None,
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

fn stamped_cache(mut cache: AccountsCache, stamp: u64) -> AccountsCache {
    for record in cache.logins.values_mut() {
        record.inputs_stamp = Some(stamp);
    }
    cache
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
        profile: None,
        key: key(kind),
        inputs_stamp: None,
        outcome: AccountProbe::Found(AgentAccount {
            plan: Some(kind.to_owned()),
            ..Default::default()
        }),
        outcome_class: ProbeOutcomeClass::Success,
        version: None,
        version_probed_at_ms: None,
        account_ms: 1,
        version_ms: 0,
        total_ms: 1,
    }
}

fn profile_account(owner: Option<&str>) -> AgentAccount {
    AgentAccount {
        profile_fingerprint: owner.map(str::to_owned),
        plan: Some("max_5x".into()),
        metered: Some(true),
        ..Default::default()
    }
}

fn profile_state(attempted_at_ms: u64, read_at_ms: u64) -> AccountProfileState {
    AccountProfileState {
        attempted_at_ms,
        answer: Some(AccountProfileAnswer {
            owner: "owner".into(),
            basis: Some("max_5x".into()),
            plan: Some("pro".into()),
            read_at_ms,
        }),
    }
}

#[test]
fn profile_answer_records_the_fresh_credential_plan() {
    let mut account = profile_account(Some("owner"));
    for basis in [Some("max_20x"), None] {
        account.plan = basis.map(str::to_owned);
        let state = read_account_profile(
            &account,
            None,
            ProfileReadPolicy::Forced,
            0,
            100,
            || AccountProfileProbe::Answered(Some("pro".into())),
            || 200,
        )
        .unwrap();
        let answer = state.answer.unwrap();
        assert_eq!(answer.basis.as_deref(), basis);
        assert_eq!(answer.plan.as_deref(), Some("pro"));
        let encoded = serde_json::to_value(&answer).unwrap();
        assert_eq!(encoded.get("basis").is_some(), basis.is_some());
        assert_eq!(
            serde_json::from_value::<AccountProfileAnswer>(encoded).unwrap(),
            answer
        );
    }
    let legacy: AccountProfileAnswer = serde_json::from_value(serde_json::json!({
        "owner": "owner", "plan": "max_5x", "read_at_ms": 1
    }))
    .unwrap();
    assert_eq!(legacy.basis, None);
}

#[test]
fn profile_basis_changes_are_due_after_the_attempt_floor() {
    let floor = ACCOUNTS_TTL.as_millis() as u64;
    let state = profile_state(100, 100);
    let mut legacy = state.clone();
    legacy.answer.as_mut().unwrap().basis = None;
    for (state, basis, due) in [
        (&state, Some("pro"), true),
        (&state, None, true),
        (&state, Some("max_5x"), false),
        (&legacy, Some("max_5x"), true),
        (&legacy, None, false),
    ] {
        assert_eq!(
            profile_read_due(false, 100 + floor, Some(state), Some("owner"), basis, 0),
            due,
            "a changed credential plan makes the answer due"
        );
        assert!(!profile_read_due(
            false,
            100 + floor - 1,
            Some(state),
            Some("owner"),
            basis,
            0
        ));
        assert!(profile_read_due(
            true,
            100,
            Some(state),
            Some("owner"),
            basis,
            0
        ));
    }
}

#[test]
fn profile_merge_does_not_mask_a_changed_credential_plan() {
    for (basis, fresh_plan, expected) in [
        (Some("max_5x"), Some("pro"), Some("pro")),
        (None, Some("pro"), Some("pro")),
        (Some("pro"), Some("pro"), Some("max_5x")),
        (None, None, Some("max_5x")),
        (Some("max_5x"), None, None),
    ] {
        let mut state = profile_state(1, 1);
        let answer = state.answer.as_mut().unwrap();
        answer.basis = basis.map(str::to_owned);
        answer.plan = Some("max_5x".into());
        let mut old = record(1, true, None);
        old.profile = Some(state);
        let previous = AccountsCache {
            logins: BTreeMap::from([(key("claude"), old)]),
        };
        let account = AgentAccount {
            plan: fresh_plan.map(str::to_owned),
            ..profile_account(Some("owner"))
        };
        let cache = merge_probe_results(
            &previous,
            &BTreeSet::new(),
            20,
            [ProviderProbeResult {
                outcome: AccountProbe::Found(account),
                ..successful_probe("claude")
            }],
        );
        assert_eq!(
            cache.logins[&key("claude")]
                .account
                .as_ref()
                .unwrap()
                .plan
                .as_deref(),
            expected,
            "an answer must not override a different credential plan"
        );
    }
}

#[test]
fn profile_due_rule_has_strict_triggers_and_a_ten_minute_attempt_floor() {
    let floor = ACCOUNTS_TTL.as_millis() as u64;
    let day = 86_400_000;
    let now = 3 * day;
    let fresh = profile_state(now - floor, now - 5_000);
    let mut switched = fresh.clone();
    switched.answer.as_mut().unwrap().owner = "other".into();
    let cases = [
        (None, 0, true),
        (
            Some(AccountProfileState {
                attempted_at_ms: now - floor,
                answer: None,
            }),
            0,
            true,
        ),
        (Some(switched), 0, true),
        (Some(profile_state(now - floor, now - day - 1)), 0, true),
        (Some(fresh.clone()), now - 1_000, true),
        (Some(fresh.clone()), now - 2_000, true),
        (Some(fresh.clone()), 0, false),
        (Some(fresh.clone()), now - 5_000, false),
        (Some(profile_state(now - floor, now - day)), 0, false),
    ];
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (state, trigger, due) in cases {
        actual.push(profile_read_due(
            false,
            now,
            state.as_ref(),
            Some("owner"),
            Some("max_5x"),
            trigger,
        ));
        expected.push(due);
        let mut recent = state.unwrap_or_default();
        recent.attempted_at_ms = now - floor + 1;
        actual.push(profile_read_due(
            false,
            now,
            Some(&recent),
            Some("owner"),
            Some("max_5x"),
            trigger,
        ));
        expected.push(false);
        actual.push(profile_read_due(
            true,
            now,
            Some(&recent),
            Some("owner"),
            Some("max_5x"),
            trigger,
        ));
        expected.push(true);
    }
    for force in [false, true] {
        actual.push(profile_read_due(force, now, None, None, None, now));
        expected.push(false);
    }
    assert_eq!(actual, expected);
}

#[test]
fn profile_merge_resolves_only_bound_answers_and_carries_state_through_every_outcome() {
    let mut old = record(1, true, Some(profile_account(Some("owner"))));
    old.account.as_mut().unwrap().plan = Some("pro".into());
    old.profile = Some(profile_state(1, 1));
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("claude"), old.clone())]),
    };
    let mut actual = Vec::new();
    for outcome in [
        AccountProbe::Found(profile_account(Some("owner"))),
        AccountProbe::Found(profile_account(Some("other"))),
        AccountProbe::Found(profile_account(None)),
        AccountProbe::LoggedOut,
        AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut),
    ] {
        let result = ProviderProbeResult {
            outcome,
            ..successful_probe("claude")
        };
        let cache = merge_probe_results(&previous, &BTreeSet::new(), 20, [result]);
        let record = &cache.logins[&key("claude")];
        actual.push((
            record
                .account
                .as_ref()
                .and_then(|account| account.plan.clone()),
            record.profile.clone(),
        ));
    }
    assert_eq!(
        actual,
        [
            Some("pro"),
            Some("max_5x"),
            Some("max_5x"),
            None,
            Some("pro")
        ]
        .into_iter()
        .map(|plan| (plan.map(str::to_owned), old.profile.clone()))
        .collect::<Vec<_>>()
    );
    for answer in [
        None,
        Some(AccountProfileAnswer {
            owner: "other".into(),
            basis: Some("max_5x".into()),
            plan: Some("pro".into()),
            read_at_ms: 1,
        }),
    ] {
        let mut previous = previous.clone();
        previous
            .logins
            .get_mut(&key("claude"))
            .unwrap()
            .profile
            .as_mut()
            .unwrap()
            .answer = answer;
        let result = ProviderProbeResult {
            outcome: AccountProbe::Found(profile_account(Some("owner"))),
            ..successful_probe("claude")
        };
        let cache = merge_probe_results(&previous, &BTreeSet::new(), 20, [result]);
        assert_eq!(
            cache.logins[&key("claude")]
                .account
                .as_ref()
                .unwrap()
                .plan
                .as_deref(),
            Some("max_5x")
        );
    }
}

#[test]
fn profile_failure_changes_only_the_attempt_and_an_unrecognised_answer_ends_override() {
    use std::cell::Cell;
    let account = profile_account(Some("owner"));
    let state = profile_state(1, 1);
    let failed = read_account_profile(
        &account,
        Some(&state),
        ProfileReadPolicy::Forced,
        0,
        100,
        || AccountProfileProbe::Failed,
        || 100,
    );
    assert!(
        failed.is_some(),
        "a failed profile read must stamp an attempt"
    );
    let mut old = record(1, true, Some(account.clone()));
    old.profile = Some(state.clone());
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("claude"), old)]),
    };
    let result = || ProviderProbeResult {
        outcome: AccountProbe::Found(account.clone()),
        ..successful_probe("claude")
    };
    let no_read = merge_probe_results(&previous, &BTreeSet::new(), 20, [result()]);
    let mut failed_result = result();
    failed_result.profile = failed;
    let mut failed_cache = merge_probe_results(&previous, &BTreeSet::new(), 20, [failed_result]);
    let failed_state = failed_cache
        .logins
        .get_mut(&key("claude"))
        .unwrap()
        .profile
        .as_mut()
        .unwrap();
    assert_eq!(failed_state.attempted_at_ms, 100);
    failed_state.attempted_at_ms = state.attempted_at_ms;
    assert_eq!(failed_cache.logins, no_read.logins);
    let first_failure = read_account_profile(
        &account,
        None,
        ProfileReadPolicy::Due,
        0,
        100,
        || AccountProfileProbe::Failed,
        || 1_500,
    )
    .unwrap();
    assert_eq!(
        first_failure.attempted_at_ms, 100,
        "the attempt takes the batch stamp, not the clock after the auth probe"
    );
    let floor = ACCOUNTS_TTL.as_millis() as u64;
    let mut cache = AccountsCache::default();
    cache
        .logins
        .insert(key("claude"), record(100, true, Some(account.clone())));
    assert!(
        due_provider_logins(
            &cache,
            &empty_snapshot(),
            &native_logins(),
            &BTreeMap::new(),
            100 + floor + 1
        )
        .contains(&key("claude")),
        "the floor expires no later than the account cadence re-probes the login"
    );
    assert!(!profile_read_due(
        false,
        100 + floor - 1,
        Some(&first_failure),
        Some("owner"),
        Some("max_5x"),
        0
    ));
    assert!(profile_read_due(
        false,
        100 + floor,
        Some(&first_failure),
        Some("owner"),
        Some("max_5x"),
        0
    ));
    let clock = Cell::new(100);
    let trigger_ms = Cell::new(0);
    let answered = read_account_profile(
        &account,
        Some(&state),
        ProfileReadPolicy::Forced,
        0,
        100,
        || {
            clock.set(150);
            trigger_ms.set(clock.get());
            clock.set(200);
            AccountProfileProbe::Answered(None)
        },
        || clock.get(),
    )
    .unwrap();
    assert_eq!(answered.attempted_at_ms, 100);
    assert_eq!(answered.answer.as_ref().unwrap().read_at_ms, 100);
    assert!(
        profile_read_due(
            false,
            100 + floor,
            Some(&answered),
            Some("owner"),
            Some("max_5x"),
            trigger_ms.get()
        ),
        "a reset stamped while the request was in flight makes the next batch due"
    );
    let mut answered_result = result();
    answered_result.profile = Some(answered);
    let cache = merge_probe_results(&previous, &BTreeSet::new(), 20, [answered_result]);
    assert_eq!(
        cache.logins[&key("claude")]
            .account
            .as_ref()
            .unwrap()
            .plan
            .as_deref(),
        Some("max_5x")
    );
    assert_eq!(
        read_account_profile(
            &account,
            Some(&state),
            ProfileReadPolicy::Forced,
            0,
            100,
            || AccountProfileProbe::NotAttempted,
            || 100
        ),
        None
    );
    for (account, policy) in [
        (profile_account(None), ProfileReadPolicy::Forced),
        (account, ProfileReadPolicy::Disabled),
    ] {
        assert_eq!(
            read_account_profile(
                &account,
                Some(&state),
                policy,
                0,
                100,
                || panic!("disabled or unidentified profile reads must not run"),
                || 100
            ),
            None
        );
    }
}

#[test]
fn unavailable_coordination_reads_profiles_only_when_forced() {
    use std::cell::Cell;
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    std::fs::create_dir_all(runtime.shared_accounts_lock()).unwrap();
    let login = ProviderLogin::default_for(key("claude").kind);
    let mut actual = Vec::new();
    for force in [false, true] {
        let calls = Cell::new(0);
        let observed = Cell::new(None);
        query_provider_accounts_with(
            &empty_snapshot(),
            &runtime,
            std::slice::from_ref(&login),
            force,
            Duration::ZERO,
            |_| None,
            |_, _, _, cache, policy| {
                observed.set(Some(policy));
                let _ = read_account_profile(
                    &profile_account(Some("owner")),
                    None,
                    policy,
                    0,
                    100,
                    || {
                        calls.set(calls.get() + 1);
                        AccountProfileProbe::Answered(Some("pro".into()))
                    },
                    || 100,
                );
                cache.clone()
            },
        );
        actual.push((observed.get(), calls.get()));
    }
    assert_eq!(
        actual,
        vec![
            (Some(ProfileReadPolicy::Disabled), 0),
            (Some(ProfileReadPolicy::Forced), 1)
        ]
    );
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
fn forced_query_waits_for_a_peer_then_probes_and_reads_once() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_shared_dirs().unwrap();
    let mut peer_cache = AccountsCache {
        logins: BTreeMap::from([(
            key("claude"),
            record(1, true, Some(profile_account(Some("owner")))),
        )]),
    };
    write_accounts_cache(&runtime.shared_accounts_path(), &peer_cache);
    peer_cache
        .logins
        .get_mut(&key("claude"))
        .unwrap()
        .probed_at_ms = 2;
    let producer = match crate::disk::single_flight::coordinate::<()>(
        &runtime.shared_accounts_lock(),
        Duration::ZERO,
        0,
        || None,
    ) {
        crate::disk::single_flight::Coordination::Produce(guard) => guard,
        _ => panic!("test must hold the account producer lock"),
    };
    let probes = AtomicUsize::new(0);
    let reads = AtomicUsize::new(0);
    let login = ProviderLogin::default_for(key("claude").kind);
    let refreshed = std::thread::scope(|scope| {
        let runtime = &runtime;
        let peer_cache = &peer_cache;
        scope.spawn(move || {
            std::thread::sleep(ACCOUNTS_WAIT_STEP * (ACCOUNTS_WAIT_STEPS + 5));
            write_accounts_cache(&runtime.shared_accounts_path(), peer_cache);
            drop(producer);
        });
        query_provider_accounts_with(
            &empty_snapshot(),
            runtime,
            std::slice::from_ref(&login),
            true,
            Duration::from_secs(3),
            |_| None,
            |_, _, due, cache, policy| {
                probes.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    cache, peer_cache,
                    "re-read the peer's publication on winning"
                );
                assert_eq!(due, &BTreeSet::from([key("claude")]));
                assert_eq!(policy, ProfileReadPolicy::Forced);
                let account = profile_account(Some("owner"));
                let profile = read_account_profile(
                    &account,
                    None,
                    policy,
                    0,
                    3,
                    || {
                        reads.fetch_add(1, Ordering::SeqCst);
                        AccountProfileProbe::Answered(Some("pro".into()))
                    },
                    || 4,
                );
                merge_probe_results(
                    cache,
                    &BTreeSet::new(),
                    3,
                    [ProviderProbeResult {
                        profile,
                        outcome: AccountProbe::Found(account),
                        ..successful_probe("claude")
                    }],
                )
            },
        )
    });
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        refreshed.logins[&key("claude")]
            .account
            .as_ref()
            .unwrap()
            .plan
            .as_deref(),
        Some("pro")
    );
    assert_eq!(
        read_accounts_cache(&runtime.shared_accounts_path()),
        refreshed
    );
}

#[test]
fn forced_query_serves_cache_when_a_peer_outlives_the_wait_bound() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_shared_dirs().unwrap();
    let cached = fresh_cache(0);
    write_accounts_cache(&runtime.shared_accounts_path(), &cached);
    let _producer = match crate::disk::single_flight::coordinate::<()>(
        &runtime.shared_accounts_lock(),
        Duration::ZERO,
        0,
        || None,
    ) {
        crate::disk::single_flight::Coordination::Produce(guard) => guard,
        _ => panic!("test must hold the account producer lock"),
    };
    let wait_bound = Duration::from_millis(500);
    let started = Instant::now();
    let observed = query_provider_accounts_with(
        &empty_snapshot(),
        &runtime,
        &native_logins(),
        true,
        wait_bound,
        |_| None,
        |_, _, _, _, _| panic!("a contending query must never probe without the lock"),
    );
    assert!(
        started.elapsed() >= wait_bound,
        "honor the forced wait bound"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(observed, cached);
    assert_eq!(read_accounts_cache(&runtime.shared_accounts_path()), cached);
}

#[test]
fn unforced_query_keeps_the_short_contention_wait() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_shared_dirs().unwrap();
    let cached = fresh_cache(0);
    write_accounts_cache(&runtime.shared_accounts_path(), &cached);
    let _producer = match crate::disk::single_flight::coordinate::<()>(
        &runtime.shared_accounts_lock(),
        Duration::ZERO,
        0,
        || None,
    ) {
        crate::disk::single_flight::Coordination::Produce(guard) => guard,
        _ => panic!("test must hold the account producer lock"),
    };
    let forced_wait = Duration::from_secs(3);
    let started = Instant::now();
    let observed = query_provider_accounts_with(
        &empty_snapshot(),
        &runtime,
        &native_logins(),
        false,
        forced_wait,
        |_| None,
        |_, _, _, _, _| panic!("an unforced contender must serve cache without probing"),
    );
    assert!(started.elapsed() >= ACCOUNTS_WAIT_STEP * ACCOUNTS_WAIT_STEPS);
    assert!(started.elapsed() < forced_wait);
    assert_eq!(observed, cached);
    assert_eq!(read_accounts_cache(&runtime.shared_accounts_path()), cached);
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
            |_, _, _, cache, _policy| {
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
        Duration::ZERO,
        |_| None,
        |_, _, kinds, cache, _policy| {
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
    let expected = stamped_cache(fresh_cache(unix_now_ms()), 7);
    write_accounts_cache(&runtime.shared_accounts_path(), &expected);
    let probes = AtomicUsize::new(0);

    let cached = query_provider_accounts_with(
        &empty_snapshot(),
        &runtime,
        &native_logins(),
        false,
        Duration::ZERO,
        |_| Some(7),
        |_, _, _, cache, _policy| {
            probes.fetch_add(1, Ordering::SeqCst);
            cache.clone()
        },
    );

    assert_eq!(cached, expected);
    assert_eq!(probes.load(Ordering::SeqCst), 0);
}

struct AccountQuery {
    _dir: tempfile::TempDir,
    runtime: RuntimePaths,
    logins: Vec<ProviderLogin>,
    probes: BTreeMap<LoginKey, AtomicUsize>,
}

impl AccountQuery {
    fn new(kinds: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_shared_dirs().unwrap();
        let logins: Vec<_> = native_logins()
            .into_iter()
            .filter(|login| kinds.contains(&login.kind().as_str()))
            .collect();
        let probes = logins
            .iter()
            .map(|login| (login.key(), AtomicUsize::new(0)))
            .collect();
        Self {
            _dir: dir,
            runtime,
            logins,
            probes,
        }
    }

    fn query(
        &self,
        mut cache: AccountsCache,
        age_ms: u64,
        stamp: Option<u64>,
        outcome: impl Fn(&str) -> AccountProbe,
    ) -> AccountsCache {
        let now_ms = unix_now_ms();
        for record in cache.logins.values_mut() {
            record.probed_at_ms = now_ms - age_ms;
        }
        write_accounts_cache(&self.runtime.shared_accounts_path(), &cache);
        query_provider_accounts_with(
            &empty_snapshot(),
            &self.runtime,
            &self.logins,
            false,
            Duration::ZERO,
            |_| stamp,
            |_, _, due, cache, _| {
                let results = due.iter().map(|key| {
                    self.probes[key].fetch_add(1, Ordering::SeqCst);
                    ProviderProbeResult {
                        key: key.clone(),
                        inputs_stamp: stamp,
                        outcome: outcome(key.kind.as_str()),
                        ..successful_probe(key.kind.as_str())
                    }
                });
                merge_probe_results(cache, &BTreeSet::new(), now_ms, results)
            },
        )
    }

    fn calls(&self, kind: &str) -> usize {
        self.probes[&key(kind)].load(Ordering::SeqCst)
    }
}

fn unavailable_probe(_: &str) -> AccountProbe {
    AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut)
}

#[test]
fn first_unavailable_probe_retries_after_ten_seconds() {
    let query = AccountQuery::new(&["copilot"]);
    let failed = query.query(AccountsCache::default(), 0, None, unavailable_probe);
    assert_eq!(failed.logins[&key("copilot")].consecutive_failures, 1);
    assert_eq!(query.calls("copilot"), 1);
    let cached = query.query(failed, 9_000, None, unavailable_probe);
    assert_eq!(query.calls("copilot"), 1);
    let retried = query.query(cached, 11_000, None, unavailable_probe);
    assert_eq!(query.calls("copilot"), 2);
    assert_eq!(retried.logins[&key("copilot")].consecutive_failures, 2);
}

#[test]
fn consecutive_unavailable_probes_back_off_to_the_success_ttl() {
    let query = AccountQuery::new(&["copilot"]);
    let mut cache = query.query(AccountsCache::default(), 0, None, unavailable_probe);
    for (index, seconds) in [10, 20, 40, 80, 160, 320, 600, 600].into_iter().enumerate() {
        let count = index + 1;
        assert_eq!(
            cache.logins[&key("copilot")].consecutive_failures,
            count as u32
        );
        assert!(
            due_provider_logins(
                &cache,
                &empty_snapshot(),
                &query.logins,
                &BTreeMap::new(),
                cache.logins[&key("copilot")].probed_at_ms + seconds * 1_000,
            )
            .is_empty(),
            "the retry deadline itself is not due"
        );
        cache = query.query(cache, seconds * 1_000 - 1_000, None, unavailable_probe);
        assert_eq!(query.calls("copilot"), count, "before {seconds}s");
        cache = query.query(cache, seconds * 1_000 + 1_000, None, unavailable_probe);
        assert_eq!(query.calls("copilot"), count + 1, "after {seconds}s");
    }
    cache
        .logins
        .get_mut(&key("copilot"))
        .unwrap()
        .consecutive_failures = u32::MAX;
    cache = query.query(cache, 599_000, None, unavailable_probe);
    assert_eq!(query.calls("copilot"), 9);
    cache = query.query(cache, 601_000, None, unavailable_probe);
    assert_eq!(query.calls("copilot"), 10);
    assert_eq!(cache.logins[&key("copilot")].consecutive_failures, u32::MAX);
}

#[test]
fn confident_probe_resets_and_omits_the_failure_count() {
    for logged_out in [false, true] {
        let query = AccountQuery::new(&["copilot"]);
        let previous = AccountsCache {
            logins: [(
                key("copilot"),
                ProviderRecord {
                    consecutive_failures: 8,
                    ..record(0, false, None)
                },
            )]
            .into(),
        };
        let recovered = query.query(previous, 601_000, None, |_| {
            if logged_out {
                AccountProbe::LoggedOut
            } else {
                AccountProbe::Found(AgentAccount::default())
            }
        });
        assert_eq!(query.calls("copilot"), 1);
        assert_eq!(recovered.logins[&key("copilot")].consecutive_failures, 0);
        let written: serde_json::Value =
            crate::disk::atomic::read_json_cache(&query.runtime.shared_accounts_path());
        assert!(
            written["logins"]["copilot@default"]
                .get("consecutive_failures")
                .is_none()
        );
        let failed = query.query(recovered, 601_000, None, unavailable_probe);
        assert_eq!(query.calls("copilot"), 2);
        assert_eq!(failed.logins[&key("copilot")].consecutive_failures, 1);
    }
}

#[test]
fn moved_inputs_stamp_bypasses_failure_backoff_after_the_retry_floor() {
    let query = AccountQuery::new(&["pi"]);
    let cache = AccountsCache {
        logins: [(
            key("pi"),
            ProviderRecord {
                consecutive_failures: u32::MAX,
                inputs_stamp: Some(7),
                ..record(0, false, None)
            },
        )]
        .into(),
    };
    let cache = query.query(cache, 11_000, Some(7), unavailable_probe);
    assert_eq!(query.calls("pi"), 0, "unchanged inputs keep the backoff");
    let cache = query.query(cache, 9_000, Some(8), unavailable_probe);
    assert_eq!(
        query.calls("pi"),
        0,
        "changed inputs still wait out the floor"
    );
    let cache = query.query(cache, 11_000, Some(8), unavailable_probe);
    assert_eq!(query.calls("pi"), 1);
    assert_eq!(cache.logins[&key("pi")].inputs_stamp, Some(8));
    assert_eq!(cache.logins[&key("pi")].consecutive_failures, u32::MAX);
}

#[test]
fn legacy_failure_count_defaults_to_the_first_retry() {
    let query = AccountQuery::new(&["copilot"]);
    let legacy: AccountsCache = serde_json::from_value(serde_json::json!({
        "logins": {"copilot@default": {"probed_at_ms": 1, "ok": false, "account": null}}
    }))
    .unwrap();
    assert_eq!(legacy.logins[&key("copilot")].consecutive_failures, 0);
    let cache = query.query(legacy, 9_000, None, unavailable_probe);
    assert_eq!(query.calls("copilot"), 0);
    let cache = query.query(cache, 11_000, None, unavailable_probe);
    assert_eq!(query.calls("copilot"), 1);
    assert_eq!(cache.logins[&key("copilot")].consecutive_failures, 1);
}

#[test]
fn one_providers_failure_count_never_delays_another_provider() {
    let query = AccountQuery::new(&["copilot", "pi"]);
    let cache = AccountsCache {
        logins: [
            (
                key("copilot"),
                ProviderRecord {
                    consecutive_failures: 8,
                    ..record(0, false, None)
                },
            ),
            (
                key("pi"),
                ProviderRecord {
                    consecutive_failures: 1,
                    ..record(0, false, None)
                },
            ),
        ]
        .into(),
    };
    let cache = query.query(cache, 11_000, None, unavailable_probe);
    assert_eq!(query.calls("pi"), 1);
    assert_eq!(query.calls("copilot"), 0);
    assert_eq!(cache.logins[&key("copilot")].consecutive_failures, 8);
    assert_eq!(cache.logins[&key("pi")].consecutive_failures, 2);
}

#[test]
fn provider_query_reprobes_only_the_login_whose_inputs_stamp_moved() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_shared_dirs().unwrap();
    let past_floor_ms = unix_now_ms().saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
    let mut cache = stamped_cache(fresh_cache(past_floor_ms), 7);
    cache.logins.get_mut(&key("claude")).unwrap().inputs_stamp = None;
    write_accounts_cache(&runtime.shared_accounts_path(), &cache);
    let asked = Mutex::new(Vec::new());
    let probed = Mutex::new(Vec::new());

    query_provider_accounts_with(
        &empty_snapshot(),
        &runtime,
        &native_logins(),
        false,
        Duration::ZERO,
        |login| {
            let kind = login.kind().as_str().to_owned();
            let stamp = if kind == "pi" { 8 } else { 7 };
            asked.lock().unwrap().push(kind);
            Some(stamp)
        },
        |_, _, due, cache, _policy| {
            probed.lock().unwrap().extend(due.iter().cloned());
            cache.clone()
        },
    );

    assert_eq!(*probed.lock().unwrap(), vec![key("pi")]);
    let asked = asked.into_inner().unwrap();
    assert!(
        !asked.iter().any(|kind| kind == "claude"),
        "a record without a stored stamp never asks its adapter"
    );
    assert_eq!(
        asked.iter().filter(|kind| *kind == "pi").count(),
        1,
        "current stamps are read once per query"
    );
}

#[test]
fn inputs_stamp_due_rule_covers_each_record_state() {
    let now_ms = unix_now_ms();
    let inside_floor_ms = now_ms;
    let past_floor_ms = now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
    let past_ttl_ms = now_ms.saturating_sub(ACCOUNTS_TTL.as_millis() as u64 + 1);
    let snapshot = empty_snapshot();
    let due = |probed_at_ms, ok, stored: Option<u64>, current: Option<u64>, provisional| {
        let mut cache = fresh_cache(now_ms);
        cache.logins.insert(
            key("pi"),
            ProviderRecord {
                inputs_stamp: stored,
                ..record(
                    probed_at_ms,
                    ok,
                    Some(AgentAccount {
                        provisional,
                        ..Default::default()
                    }),
                )
            },
        );
        let stamps = current
            .map(|stamp| (key("pi"), stamp))
            .into_iter()
            .collect();
        due_provider_logins(&cache, &snapshot, &native_logins(), &stamps, now_ms)
            .contains(&key("pi"))
    };

    for (probed_at_ms, ok, stored, current, expected, why) in [
        (
            past_floor_ms,
            true,
            Some(1),
            Some(2),
            true,
            "a moved stamp past the floor",
        ),
        (
            inside_floor_ms,
            true,
            Some(1),
            Some(2),
            false,
            "a moved stamp inside the floor",
        ),
        (
            past_floor_ms,
            true,
            Some(1),
            Some(1),
            false,
            "an unchanged stamp",
        ),
        (past_floor_ms, true, None, Some(2), false, "no stored stamp"),
        (
            past_floor_ms,
            true,
            Some(1),
            None,
            false,
            "no current stamp",
        ),
        (
            past_ttl_ms,
            true,
            Some(1),
            Some(1),
            true,
            "an unchanged stamp past the TTL",
        ),
        (
            past_floor_ms,
            false,
            Some(1),
            Some(1),
            true,
            "a failed probe past the floor",
        ),
        (
            inside_floor_ms,
            false,
            Some(1),
            Some(2),
            false,
            "a failed probe inside the floor",
        ),
    ] {
        assert_eq!(
            due(probed_at_ms, ok, stored, current, false),
            expected,
            "{why}"
        );
    }
    assert_eq!(
        [
            due(
                now_ms - ACCOUNTS_RETRY_TTL.as_millis() as u64,
                true,
                Some(1),
                Some(1),
                true
            ),
            due(past_floor_ms, true, Some(1), Some(1), true),
            due(past_floor_ms, true, None, None, true),
            due(past_floor_ms, true, Some(1), Some(1), false),
        ],
        [false, true, true, false]
    );
}

#[test]
fn provisional_reprobes_reuse_versions_only_until_the_original_version_read_expires() {
    let ttl = ACCOUNTS_TTL.as_millis() as u64;
    let now = ttl * 3;
    let calls = AtomicUsize::new(0);
    let mut previous = record(
        now - 10_001,
        true,
        Some(AgentAccount {
            provisional: true,
            version: Some("1.4.0".into()),
            ..Default::default()
        }),
    );
    let outcomes: Vec<_> = [now - ttl + 1, now - ttl, now - ttl - 1]
        .into_iter()
        .map(|read_at| {
            previous.version_probed_at_ms = Some(read_at);
            probe_account_version(
                &AccountProbe::Found(AgentAccount::default()),
                false,
                Some(&previous),
                now,
                || {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Some("1.5.0".to_owned())
                },
            )
        })
        .collect();
    assert_eq!(
        (outcomes, calls.load(Ordering::Relaxed)),
        (
            vec![None, Some("1.5.0".to_owned()), Some("1.5.0".to_owned())],
            2
        )
    );
    previous.version_probed_at_ms = Some(now - 1);
    let merged = probe_accounts_with(
        &BTreeSet::from([key("pi")]),
        &AccountsCache {
            logins: BTreeMap::from([(key("pi"), previous)]),
        },
        &BTreeSet::new(),
        now,
        |_, _| {
            Some((
                AccountProbe::Found(AgentAccount {
                    provisional: true,
                    ..Default::default()
                }),
                None,
            ))
        },
    );
    assert_eq!(
        merged.logins[&key("pi")].version_probed_at_ms,
        Some(now - 1)
    );
}

#[test]
fn merge_stores_each_probed_logins_pre_probe_stamp_and_keeps_the_rest() {
    let previous = stamped_cache(fresh_cache(10), 1);
    let probed = |kind: &str, outcome: AccountProbe, outcome_class| ProviderProbeResult {
        inputs_stamp: Some(2),
        outcome,
        outcome_class,
        ..successful_probe(kind)
    };
    let merged = merge_probe_results(
        &previous,
        &BTreeSet::new(),
        20,
        [
            successful_probe("pi"),
            probed(
                "codex",
                AccountProbe::LoggedOut,
                ProbeOutcomeClass::LoggedOut,
            ),
            probed(
                "claude",
                AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut),
                ProbeOutcomeClass::Unavailable,
            ),
            ProviderProbeResult {
                inputs_stamp: Some(2),
                ..successful_probe("amp")
            },
        ],
    );

    let stamp = |kind| merged.logins[&key(kind)].inputs_stamp;
    assert_eq!(
        stamp("pi"),
        None,
        "a probe without a stamp clears the stored one"
    );
    assert_eq!(stamp("codex"), Some(2), "logged out");
    assert_eq!(stamp("claude"), Some(2), "unavailable");
    assert_eq!(stamp("amp"), Some(2), "found");
    assert_eq!(
        stamp("kimi"),
        Some(1),
        "a login outside the batch keeps its stamp"
    );
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
    let due = due_provider_logins(
        &cache,
        &snapshot,
        &native_logins(),
        &BTreeMap::new(),
        now_ms,
    );
    assert_eq!(due, BTreeSet::from([key("copilot")]));

    let successful = cache.logins[&key("claude")].clone();
    let mut probed = Vec::new();
    let merged = probe_accounts_with(&due, &cache, &BTreeSet::new(), now_ms, |kind, _active| {
        probed.push(kind.to_owned());
        Some((
            AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut),
            None,
        ))
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
        |_kind, _active| {
            Some((
                AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut),
                None,
            ))
        },
    );

    assert_eq!(
        merged.logins[&key("copilot")].account,
        Some(previous_account)
    );
    assert!(!merged.logins[&key("copilot")].ok);
    assert_eq!(
        serde_json::to_value(&merged.logins[&key("copilot")]).unwrap()["probe_error"],
        serde_json::json!("timed_out")
    );

    for outcome in [
        AccountProbe::LoggedOut,
        AccountProbe::Found(AgentAccount::default()),
    ] {
        let mut outcome = Some(outcome);
        let recovered = probe_accounts_with(
            &BTreeSet::from([key("copilot")]),
            &merged,
            &BTreeSet::new(),
            30,
            |_, _| Some((outcome.take().unwrap(), None)),
        );
        assert!(
            serde_json::to_value(&recovered.logins[&key("copilot")]).unwrap()["probe_error"]
                .is_null()
        );
    }

    let active = probe_accounts_with(
        &BTreeSet::from([key("copilot")]),
        &previous,
        &BTreeSet::from(["copilot".to_owned()]),
        20,
        |_kind, _active| {
            Some((
                AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut),
                Some("1.0.0".to_owned()),
            ))
        },
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
    // The kept version makes the account `Some`, so the record says which
    // outcome it holds and every status reader sees the logout.
    use crate::agents::account::{ProviderStatus, RecordedLogin};
    let status = |cache: &AccountsCache| ProviderStatus::from_record(cache.logins.get(&key("pi")));
    assert_eq!(
        found.logins[&key("pi")].login,
        Some(RecordedLogin::LoggedIn)
    );
    assert_eq!(
        logged_out.logins[&key("pi")].login,
        Some(RecordedLogin::LoggedOut)
    );
    assert_eq!(status(&logged_out), ProviderStatus::LoggedOut);

    let idle = probe_accounts_with(&due, &previous, &BTreeSet::new(), 20, |_kind, _active| {
        Some((AccountProbe::LoggedOut, None))
    });
    assert_eq!(idle.logins[&key("pi")].account, None);
    assert_eq!(status(&idle), ProviderStatus::LoggedOut);

    // A login the provider reports with no facts keeps the version too, and
    // stays a login.
    let bare = probe_accounts_with(&due, &previous, &active, 20, |_kind, _active| {
        Some((AccountProbe::Found(AgentAccount::default()), None))
    });
    assert_eq!(
        bare.logins[&key("pi")].account,
        logged_out.logins[&key("pi")].account,
        "the two outcomes store the same account"
    );
    assert_eq!(status(&bare), ProviderStatus::LoggedIn);

    // A failed probe records no outcome: it keeps what it had and retries.
    let failed = probe_accounts_with(&due, &logged_out, &active, 30, |_kind, _active| {
        Some((
            AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut),
            None,
        ))
    });
    assert_eq!(failed.logins[&key("pi")].login, None);
    assert!(!failed.logins[&key("pi")].ok);
    assert_eq!(status(&failed), ProviderStatus::Unavailable);
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
    let versions = BTreeMap::from([(key("codex"), "0.135.0".to_owned())]);

    let merged = accounts_with_context_versions(&cache, &versions, &RoomLoginSet::native());
    let persisted = read_accounts_cache(&path);

    assert_eq!(persisted.logins[&key("codex")].probed_at_ms, 42);
    assert_eq!(
        merged
            .get(&key("codex"))
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
        Some(crate::ids::RoomLogins::from([(work_key.kind.clone(), work_key.name.clone())]).into()),
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
        cached_accounts_for_snapshot(&runtime, &empty_snapshot(), &work)[&work_key]
            .plan
            .as_deref(),
        Some("work")
    );
    let mixed = work.with_agents(&[root_agent("claude", "native", None)]);
    let projected = cached_accounts_for_snapshot(&runtime, &empty_snapshot(), &mixed);
    assert_eq!(projected[&key("claude")].plan.as_deref(), Some("updated"));
    assert_eq!(projected[&work_key].plan.as_deref(), Some("work"));
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
        due_provider_logins(&cache, &snapshot, &native_logins(), &BTreeMap::new(), 100),
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
            !due_provider_logins(
                &cache,
                &snapshot,
                &native_logins(),
                &BTreeMap::new(),
                now_ms
            )
            .contains(&key(kind))
        );

        cache.logins.get_mut(&key(kind)).unwrap().probed_at_ms =
            now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
        assert!(
            due_provider_logins(
                &cache,
                &snapshot,
                &native_logins(),
                &BTreeMap::new(),
                now_ms
            )
            .contains(&key(kind)),
            "an active {kind} account without a version re-probes after the retry window"
        );

        cache.logins.get_mut(&key(kind)).unwrap().ok = false;
        cache.logins.get_mut(&key(kind)).unwrap().probed_at_ms = now_ms;
        assert!(
            !due_provider_logins(
                &cache,
                &snapshot,
                &native_logins(),
                &BTreeMap::new(),
                now_ms
            )
            .contains(&key(kind)),
            "a failed {kind} probe waits for its own failure TTL"
        );
    }
}

#[test]
fn whitespace_only_reported_version_does_not_stop_the_version_reprobe() {
    let snapshot_reporting = |version: &str| {
        let mut state = root_agent("codex", "active", None);
        let mut context = crate::agents::AgentContext::new("codex", Timestamp::now());
        context.agent_version = Some(version.to_owned());
        state.context = Some(context);
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/provider-version")),
            vec![state],
            Timestamp::now(),
        )
    };
    let now_ms = unix_now_ms();
    let mut cache = fresh_cache(now_ms);
    cache.logins.insert(
        key("codex"),
        record(
            now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1),
            true,
            Some(AgentAccount {
                plan: Some("Pro".to_owned()),
                ..Default::default()
            }),
        ),
    );
    let due = |version: &str| {
        due_provider_logins(
            &cache,
            &snapshot_reporting(version),
            &native_logins(),
            &BTreeMap::new(),
            now_ms,
        )
        .contains(&key("codex"))
    };

    assert!(
        due(" "),
        "a blank reported version leaves the login without one, so it re-probes after the retry window"
    );
    assert!(
        !due("0.160.10"),
        "a session reporting a real version needs no version re-probe"
    );
}

#[test]
fn newest_version_wins_across_sessions_and_the_cached_account() {
    let session = |id: &str, version: &str, observed_secs: i64| {
        let mut state = root_agent("codex", id, None);
        let mut context = crate::agents::AgentContext::new(
            "codex",
            Timestamp::from_second(observed_secs).unwrap(),
        );
        context.agent_version = Some(version.to_owned());
        state.context = Some(context);
        state
    };
    for (old_at, new_at) in [(200, 100), (100, 200)] {
        let snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/provider-version")),
            vec![
                session("old", "0.160.9", old_at),
                session("new", "0.160.10", new_at),
            ],
            Timestamp::now(),
        );
        assert_eq!(
            context_versions(&snapshot),
            BTreeMap::from([(key("codex"), "0.160.10".to_owned())]),
            "old session observed at {old_at}, new at {new_at}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("accounts.json");
    let cached = AccountsCache {
        logins: BTreeMap::from([(
            key("codex"),
            record(
                42,
                true,
                Some(AgentAccount {
                    version: Some("0.160.10".to_owned()),
                    ..Default::default()
                }),
            ),
        )]),
    };
    write_accounts_cache(&path, &cached);
    let cache = read_accounts_cache(&path);
    let merged_version = |context: &str| {
        accounts_with_context_versions(
            &cache,
            &BTreeMap::from([(key("codex"), context.to_owned())]),
            &RoomLoginSet::native(),
        )[&key("codex")]
            .version
            .clone()
    };

    assert_eq!(
        merged_version("0.160.9").as_deref(),
        Some("0.160.10"),
        "an older live session does not overwrite the newer cached version"
    );
    assert_eq!(merged_version("0.161.0").as_deref(), Some("0.161.0"));
    assert_eq!(
        read_accounts_cache(&path),
        cached,
        "the merge is frame-local"
    );
}
