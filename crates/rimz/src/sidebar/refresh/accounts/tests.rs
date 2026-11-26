use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use jiff::Timestamp;

use super::*;
use crate::agents::{AgentState, ProviderAccountScope};
use crate::ids::WorkspaceId;
use crate::sidebar::test_support::{activity_row, root_agent, worktree_group};
use crate::store::snapshot::{SidebarProviderPanel, SidebarSnapshot};

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
        login_at_ms: None,
        stamps: Default::default(),
        live_credentials: Default::default(),
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
            plan_name: None,
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
            || AccountProfileProbe::Answered {
                plan: Some("pro".into()),
                display_name: Some("Pro".into()),
            },
            || 200,
        )
        .unwrap();
        let answer = state.answer.unwrap();
        assert_eq!(answer.basis.as_deref(), basis);
        assert_eq!(answer.plan.as_deref(), Some("pro"));
        assert_eq!(answer.plan_name.as_deref(), Some("Pro"));
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
        answer.plan_name = Some("Max".into());
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
        assert_eq!(
            cache.logins[&key("claude")]
                .account
                .as_ref()
                .unwrap()
                .plan_name
                .as_deref(),
            (basis == fresh_plan).then_some("Max"),
            "a name must follow the same saved-plan binding"
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
    old.account.as_mut().unwrap().plan_name = Some("Pro".into());
    old.profile = Some(profile_state(1, 1));
    old.profile
        .as_mut()
        .unwrap()
        .answer
        .as_mut()
        .unwrap()
        .plan_name = Some("Pro".into());
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
            record
                .account
                .as_ref()
                .and_then(|account| account.plan_name.clone()),
            record.profile.clone(),
        ));
    }
    assert_eq!(
        actual,
        [
            (Some("pro"), Some("Pro")),
            (Some("max_5x"), None),
            (Some("max_5x"), None),
            (None, None),
            (Some("pro"), Some("Pro"))
        ]
        .into_iter()
        .map(|(plan, name)| (
            plan.map(str::to_owned),
            name.map(str::to_owned),
            old.profile.clone()
        ))
        .collect::<Vec<_>>()
    );
    for answer in [
        None,
        Some(AccountProfileAnswer {
            owner: "other".into(),
            basis: Some("max_5x".into()),
            plan: Some("pro".into()),
            plan_name: Some("Pro".into()),
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
        let account = cache.logins[&key("claude")].account.as_ref().unwrap();
        assert_eq!(account.plan.as_deref(), Some("max_5x"));
        assert_eq!(account.plan_name, None);
    }
}

#[test]
fn profile_merge_uses_names_for_unrecognised_tiers_and_accepts_legacy_records() {
    let legacy = serde_json::json!({
        "account": {"plan": "max_5x", "profile_fingerprint": "owner"},
        "probed_at_ms": 1, "ok": true,
        "profile": {"attempted_at_ms": 1, "answer": {
            "owner": "owner", "basis": "max_5x", "plan": null, "read_at_ms": 1
        }}
    });
    let mut actual = Vec::new();
    for name in [None, Some("Future")] {
        let mut old: ProviderRecord = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(old.account.as_ref().unwrap().plan_name, None);
        assert_eq!(
            old.profile
                .as_ref()
                .unwrap()
                .answer
                .as_ref()
                .unwrap()
                .plan_name,
            None
        );
        old.profile
            .as_mut()
            .unwrap()
            .answer
            .as_mut()
            .unwrap()
            .plan_name = name.map(str::to_owned);
        let cache = merge_probe_results(
            &AccountsCache {
                logins: BTreeMap::from([(key("claude"), old)]),
            },
            &BTreeSet::new(),
            20,
            [ProviderProbeResult {
                outcome: AccountProbe::Found(profile_account(Some("owner"))),
                ..successful_probe("claude")
            }],
        );
        let account = cache.logins[&key("claude")].account.as_ref().unwrap();
        actual.push((
            account.plan.as_deref().unwrap().to_owned(),
            account.plan_name.clone(),
        ));
    }
    assert_eq!(
        actual,
        [
            ("max_5x".into(), None),
            ("max_5x".into(), Some("Future".into()))
        ]
    );
}

#[test]
fn profile_merge_refines_a_team_plan_to_its_seat_tier() {
    let old: ProviderRecord = serde_json::from_value(serde_json::json!({
        "account": {"plan": "team", "profile_fingerprint": "owner"},
        "probed_at_ms": 1, "ok": true,
        "profile": {"attempted_at_ms": 1, "answer": {
            "owner": "owner", "basis": "team", "plan": "team_standard", "read_at_ms": 1
        }}
    }))
    .unwrap();
    let cache = merge_probe_results(
        &AccountsCache {
            logins: BTreeMap::from([(key("claude"), old)]),
        },
        &BTreeSet::new(),
        20,
        [ProviderProbeResult {
            outcome: AccountProbe::Found(AgentAccount {
                plan: Some("team".into()),
                ..profile_account(Some("owner"))
            }),
            ..successful_probe("claude")
        }],
    );
    let account = cache.logins[&key("claude")].account.as_ref().unwrap();
    assert_eq!(account.plan.as_deref(), Some("team_standard"));
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
            AccountProfileProbe::Answered {
                plan: None,
                display_name: None,
            }
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
            |_, _, _, _, policy| {
                observed.set(Some(policy));
                let _ = read_account_profile(
                    &profile_account(Some("owner")),
                    None,
                    policy,
                    0,
                    100,
                    || {
                        calls.set(calls.get() + 1);
                        AccountProfileProbe::Answered {
                            plan: Some("pro".into()),
                            display_name: None,
                        }
                    },
                    || 100,
                );
                ProbeRun {
                    probed_at_ms: 0,
                    results: Vec::new(),
                }
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
fn account_probe_pool_probes_each_login_once_under_the_worker_ceiling() {
    let due: Vec<_> = crate::agents::known_kinds()
        .filter_map(|kind| {
            let kind = crate::ids::AgentKind::new_unchecked(kind);
            let spare = ProviderLogin::named(
                kind.clone(),
                "spare".parse().unwrap(),
                PathBuf::from(format!("/tmp/{kind}-spare")),
            )
            .ok()?;
            Some([ProviderLogin::default_for(kind), spare])
        })
        .take(8)
        .flatten()
        .collect();
    assert_eq!(due.len(), 16, "eight kinds that take a named login");
    let barrier = Arc::new(Barrier::new(MAX_PARALLEL_ACCOUNT_PROBES));
    let first_wave = AtomicUsize::new(0);
    let active = AtomicUsize::new(0);
    let maximum = AtomicUsize::new(0);
    let calls = Mutex::new(BTreeMap::<LoginKey, usize>::new());

    let batch = execute_account_probes(
        &due,
        &BTreeSet::new(),
        MAX_PARALLEL_ACCOUNT_PROBES,
        |login, _active| {
            *calls.lock().unwrap().entry(login.key()).or_default() += 1;
            let live = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(live, Ordering::SeqCst);
            if first_wave.fetch_add(1, Ordering::SeqCst) < MAX_PARALLEL_ACCOUNT_PROBES {
                barrier.wait();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            Some(ProviderProbeResult {
                key: login.key(),
                ..successful_probe(login.kind().as_str())
            })
        },
    );

    assert_eq!(batch.worker_count, MAX_PARALLEL_ACCOUNT_PROBES);
    assert_eq!(
        calls.into_inner().unwrap(),
        due.iter()
            .map(|login| (login.key(), 1))
            .collect::<BTreeMap<_, _>>()
    );
    assert_eq!(batch.results.len(), due.len());
    assert_eq!(maximum.load(Ordering::SeqCst), MAX_PARALLEL_ACCOUNT_PROBES);

    let merged = merge_probe_results(
        &AccountsCache::default(),
        &BTreeSet::new(),
        10,
        batch.results,
    );
    assert_eq!(
        merged.logins.into_keys().collect::<Vec<_>>(),
        due.iter()
            .map(ProviderLogin::key)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
        "every login publishes under its own key, in key order whatever order workers finish in"
    );
}

#[test]
fn account_probes_queue_each_kind_in_order_while_kinds_overlap() {
    let due = crate::sidebar::test_support::kind_queue_logins();
    let probes = crate::sidebar::test_support::KindProbes::new(&due);
    let batch = execute_account_probes(&due, &BTreeSet::new(), usize::MAX, |login, _active| {
        probes.probe(login);
        Some(ProviderProbeResult {
            key: login.key(),
            ..successful_probe(login.kind().as_str())
        })
    });

    probes.assert_queued(&due);
    assert_eq!(batch.worker_count, 3, "one worker per kind");
    assert_eq!(
        batch
            .results
            .iter()
            .map(|result| result.key.clone())
            .collect::<Vec<_>>(),
        due.iter().map(ProviderLogin::key).collect::<Vec<_>>(),
        "results keep the due list's order"
    );
}

#[test]
fn missing_worker_result_preserves_prior_record() {
    let mut due: Vec<_> = ["ok", "panic", "claude"]
        .into_iter()
        .map(|kind| ProviderLogin::default_for(key(kind).kind))
        .collect();
    let split_spare = ProviderLogin::named(
        key("claude").kind,
        "spare".parse().unwrap(),
        PathBuf::from("/tmp/claude-spare"),
    )
    .unwrap();
    due.push(split_spare.clone());
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
    let batch = execute_account_probes(&due, &BTreeSet::new(), usize::MAX, |login, _active| {
        let kind = login.kind().as_str();
        assert_ne!(kind, "panic", "injected worker failure");
        assert_ne!(
            login.key(),
            split_spare.key(),
            "injected failure after a finished login"
        );
        Some(successful_probe(kind))
    });
    let merged = merge_probe_results(&previous, &BTreeSet::new(), 20, batch.results);

    assert_eq!(merged.logins[&key("panic")], prior);
    assert_eq!(merged.logins[&key("ok")].probed_at_ms, 20);
    assert_eq!(
        merged
            .logins
            .get(&key("claude"))
            .map(|record| record.probed_at_ms),
        Some(20),
        "a login its kind finished before the panic keeps its result"
    );
}

#[test]
fn forced_wait_covers_the_hosts_whole_batch_and_grows_with_the_longest_queue() {
    let set = RoomLoginSet::native();
    let per_login =
        (INFORMATIONAL_PROBE_TIMEOUT * 2 + Duration::from_secs(OAUTH_HTTP_TIMEOUT_SECS)) * 2;
    let merge_base = per_login
        * crate::agents::known_kinds()
            .count()
            .div_ceil(MAX_PARALLEL_ACCOUNT_PROBES) as u32;
    let one_each = native_logins();
    let mut deeper = one_each.clone();
    deeper.extend(["work", "home"].map(|name| {
        ProviderLogin::named(
            key("claude").kind,
            name.parse().unwrap(),
            PathBuf::from(format!("/tmp/claude-{name}")),
        )
        .unwrap()
    }));
    let one_each_wait = forced_account_wait(&one_each, &set);

    assert_eq!(
        (
            one_each_wait >= merge_base,
            forced_account_wait(&one_each[..1], &set) >= merge_base,
            forced_account_wait(&deeper, &set) >= one_each_wait + per_login * 2,
        ),
        (true, true, true),
        "(one login per kind waits out the merge base's bound, so does a one-login re-probe, \
         two more logins of one kind add two login budgets)"
    );
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
                        AccountProfileProbe::Answered {
                            plan: Some("pro".into()),
                            display_name: None,
                        }
                    },
                    || 4,
                );
                ProbeRun {
                    probed_at_ms: 3,
                    results: vec![ProviderProbeResult {
                        profile,
                        outcome: AccountProbe::Found(account),
                        ..successful_probe("claude")
                    }],
                }
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
            |_, _, _, _, _policy| {
                probes.fetch_add(1, Ordering::SeqCst);
                ProbeRun {
                    probed_at_ms: 0,
                    results: Vec::new(),
                }
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
        |_, _, kinds, _, _policy| {
            due.lock().unwrap().push(kinds.clone());
            ProbeRun {
                probed_at_ms: 0,
                results: Vec::new(),
            }
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
        |_, _, _, _, _policy| {
            probes.fetch_add(1, Ordering::SeqCst);
            ProbeRun {
                probed_at_ms: 0,
                results: Vec::new(),
            }
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
            |_, _, due, _, _| {
                let results = due.iter().map(|key| {
                    self.probes[key].fetch_add(1, Ordering::SeqCst);
                    ProviderProbeResult {
                        key: key.clone(),
                        inputs_stamp: stamp,
                        outcome: outcome(key.kind.as_str()),
                        ..successful_probe(key.kind.as_str())
                    }
                });
                ProbeRun {
                    probed_at_ms: now_ms,
                    results: results.collect(),
                }
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
        |_, _, due, _, _policy| {
            probed.lock().unwrap().extend(due.iter().cloned());
            ProbeRun {
                probed_at_ms: 0,
                results: Vec::new(),
            }
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

    // A failed probe keeps the last answer and its time, while the attempt
    // fields and the backoff advance as before.
    let timed_out = |_kind: &str, _active: bool| {
        Some((
            AccountProbe::Unavailable(crate::agents::account::AccountProbeError::TimedOut),
            None,
        ))
    };
    assert_eq!(logged_out.logins[&key("pi")].login_at_ms, Some(20));
    let failed = probe_accounts_with(&due, &logged_out, &active, 30, timed_out);
    let again = probe_accounts_with(&due, &failed, &active, 40, timed_out);
    for (cache, at, failures) in [(&failed, 30, 1), (&again, 40, 2)] {
        let record = &cache.logins[&key("pi")];
        assert_eq!(record.login, Some(RecordedLogin::LoggedOut), "{at}");
        assert_eq!(
            record.login_at_ms,
            Some(20),
            "the first answer's time at {at}"
        );
        assert!(!record.ok);
        assert_eq!(record.probed_at_ms, at);
        assert_eq!(record.consecutive_failures, failures);
        assert_eq!(
            record.probe_error,
            Some(crate::agents::account::AccountProbeError::TimedOut)
        );
        assert_eq!(status(cache), ProviderStatus::Unavailable);
    }

    // A failure with no earlier answer (here an older build's ambiguous
    // record) keeps nothing.
    let unanswered = probe_accounts_with(&due, &previous, &active, 30, timed_out);
    assert_eq!(unanswered.logins[&key("pi")].login, None);
    assert_eq!(unanswered.logins[&key("pi")].login_at_ms, None);
}

fn shared_runtime(dir: &tempfile::TempDir) -> RuntimePaths {
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_shared_dirs().unwrap();
    runtime
}

fn reporting(sessions: Vec<AgentState>) -> SidebarSnapshot {
    SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/provider-version")),
        sessions,
        Timestamp::now(),
    )
}

fn versioned_session(id: &str, version: &str, observed_secs: i64) -> AgentState {
    let mut state = root_agent("codex", id, None);
    let mut context =
        crate::agents::AgentContext::new("codex", Timestamp::from_second(observed_secs).unwrap());
    context.agent_version = Some(version.to_owned());
    state.context = Some(context);
    state
}

#[test]
fn live_context_versions_merge_into_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let path = runtime.shared_accounts_path();
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

    merge_live_accounts(
        &reporting(vec![versioned_session("live", "0.135.0", 100)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    let persisted = read_accounts_cache(&path);

    assert_eq!(persisted.logins[&key("codex")].probed_at_ms, 42);
    let account = persisted.logins[&key("codex")].account.as_ref().unwrap();
    assert_eq!(account.version.as_deref(), Some("0.135.0"));
    assert_eq!(account.metered, Some(true));
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
        cached_accounts_for_snapshot(&runtime, &work)[&work_key]
            .plan
            .as_deref(),
        Some("work")
    );
    let mixed = work.with_agents(&[root_agent("claude", "native", None)]);
    let projected = cached_accounts_for_snapshot(&runtime, &mixed);
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
    let now_ms = unix_now_ms();
    let due = |version: &str| {
        let dir = tempfile::tempdir().unwrap();
        let runtime = shared_runtime(&dir);
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
        write_accounts_cache(&runtime.shared_accounts_path(), &cache);
        let snapshot = reporting(vec![versioned_session("active", version, 100)]);
        merge_live_accounts(&snapshot, &runtime, &RoomLoginSet::native());
        due_provider_logins(
            &read_accounts_cache(&runtime.shared_accounts_path()),
            &snapshot,
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
    let merged_version = |cached: Option<&str>, sessions: Vec<AgentState>| {
        let dir = tempfile::tempdir().unwrap();
        let runtime = shared_runtime(&dir);
        let account = AgentAccount {
            version: cached.map(str::to_owned),
            ..Default::default()
        };
        write_accounts_cache(
            &runtime.shared_accounts_path(),
            &AccountsCache {
                logins: BTreeMap::from([(key("codex"), record(42, true, Some(account)))]),
            },
        );
        merge_live_accounts(&reporting(sessions), &runtime, &RoomLoginSet::native());
        read_accounts_cache(&runtime.shared_accounts_path()).logins[&key("codex")]
            .account
            .as_ref()
            .and_then(|account| account.version.clone())
    };
    for (old_at, new_at) in [(200, 100), (100, 200)] {
        assert_eq!(
            merged_version(
                None,
                vec![
                    versioned_session("old", "0.160.9", old_at),
                    versioned_session("new", "0.160.10", new_at),
                ],
            )
            .as_deref(),
            Some("0.160.10"),
            "old session observed at {old_at}, new at {new_at}"
        );
    }

    assert_eq!(
        merged_version(
            Some("0.160.10"),
            vec![versioned_session("old", "0.160.9", 300)]
        )
        .as_deref(),
        Some("0.160.10"),
        "an older live session does not overwrite the newer recorded version"
    );
    assert_eq!(
        merged_version(
            Some("0.160.10"),
            vec![versioned_session("new", "0.161.0", 300)]
        )
        .as_deref(),
        Some("0.161.0")
    );
}

fn live_session(kind: &str, account: Option<AgentAccount>, observed_ms: i64) -> AgentState {
    let mut state = root_agent(kind, &format!("{kind}-{observed_ms}"), None);
    let mut context =
        crate::agents::AgentContext::new(kind, Timestamp::from_millisecond(observed_ms).unwrap());
    context.account = account;
    state.context = Some(context);
    state
}

fn stamped(account: AgentAccount, at_ms: u64) -> ProviderRecord {
    ProviderRecord {
        stamps: AccountStamps {
            plan_at_ms: account.plan.as_ref().map(|_| at_ms),
            metered_at_ms: account.metered.map(|_| at_ms),
            account_id_at_ms: account.account_id.as_ref().map(|_| at_ms),
            account_id_from_probe: account.account_id.is_some(),
            ..Default::default()
        },
        ..record(at_ms, true, Some(account))
    }
}

fn stored_account(runtime: &RuntimePaths, kind: &str) -> AgentAccount {
    read_accounts_cache(&runtime.shared_accounts_path()).logins[&key(kind)]
        .account
        .clone()
        .unwrap_or_default()
}

#[test]
fn a_plan_read_more_than_a_day_ago_still_shows_on_the_panel() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let now = crate::utils::time::unix_now_ms();
    let plan_at = now - 2 * 86_400_000;
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(
                key("codex"),
                ProviderRecord {
                    probed_at_ms: now,
                    ..stamped(
                        AgentAccount {
                            plan: Some("pro".to_owned()),
                            metered: Some(true),
                            ..Default::default()
                        },
                        plan_at,
                    )
                },
            )]),
        },
    );
    let accounts = read_provider_accounts(&runtime)
        .logins
        .into_iter()
        .filter_map(|(key, record)| Some((key, record.display_account()?)))
        .collect();
    let panels = crate::sidebar::enrich::provider_panels_from_caches(
        &runtime,
        &RoomLoginSet::native(),
        &crate::config::MachineConfig::default(),
        accounts,
        &Default::default(),
    );
    let panel = panels
        .iter()
        .find(|panel| panel.kind == "codex")
        .expect("the stored login has a panel");
    assert_eq!(
        panel.plan.as_ref().map(|plan| plan.tier.as_str()),
        Some("pro")
    );
}

#[test]
fn probe_without_a_plan_keeps_the_stored_plan_and_a_newer_probe_replaces_it() {
    let stored = AccountsCache {
        logins: BTreeMap::from([(
            key("codex"),
            record(
                1_000,
                true,
                Some(AgentAccount {
                    plan: Some("pro".to_owned()),
                    metered: Some(true),
                    ..Default::default()
                }),
            ),
        )]),
    };
    let probe = |plan: Option<&str>| ProviderProbeResult {
        outcome: AccountProbe::Found(AgentAccount {
            plan: plan.map(str::to_owned),
            metered: Some(true),
            ..Default::default()
        }),
        ..successful_probe("codex")
    };
    let plan = |cache: &AccountsCache| {
        cache.logins[&key("codex")]
            .account
            .as_ref()
            .and_then(|account| account.plan.clone())
    };

    let planless = merge_probe_results(&stored, &BTreeSet::new(), 2_000, [probe(None)]);
    assert_eq!(
        plan(&planless).as_deref(),
        Some("pro"),
        "a probe stating no plan"
    );
    let failed = merge_probe_results(
        &stored,
        &BTreeSet::new(),
        2_000,
        [ProviderProbeResult {
            outcome: unavailable_probe("codex"),
            ..successful_probe("codex")
        }],
    );
    assert_eq!(plan(&failed).as_deref(), Some("pro"), "a probe that errors");
    let newer = merge_probe_results(&planless, &BTreeSet::new(), 3_000, [probe(Some("plus"))]);
    assert_eq!(plan(&newer).as_deref(), Some("plus"));
}

#[test]
fn a_credential_file_older_than_a_live_reading_changes_nothing() {
    let live = stamped(
        AgentAccount {
            plan: Some("live-plan".to_owned()),
            ..Default::default()
        },
        5_000,
    );
    let cache = AccountsCache {
        logins: BTreeMap::from([(key("codex"), live)]),
    };
    let plan_from_file_written_at = |mtime_ms: u64| {
        let merged = merge_probe_results(
            &cache,
            &BTreeSet::new(),
            10_000,
            [ProviderProbeResult {
                outcome: AccountProbe::Found(AgentAccount {
                    plan: Some("file-plan".to_owned()),
                    credentials_updated_at_ms: Some(mtime_ms),
                    ..Default::default()
                }),
                ..successful_probe("codex")
            }],
        );
        merged.logins[&key("codex")]
            .account
            .as_ref()
            .unwrap()
            .plan
            .clone()
    };

    assert_eq!(
        plan_from_file_written_at(4_000).as_deref(),
        Some("live-plan")
    );
    assert_eq!(
        plan_from_file_written_at(6_000).as_deref(),
        Some("file-plan")
    );
}

#[test]
fn live_merge_writes_plan_metering_and_version_and_an_accountless_context_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let path = runtime.shared_accounts_path();
    write_accounts_cache(
        &path,
        &AccountsCache {
            logins: BTreeMap::from([(key("codex"), record(42, true, None))]),
        },
    );
    let before = std::fs::read(&path).unwrap();
    merge_live_accounts(
        &reporting(vec![live_session("codex", None, 1_000)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a context with no account"
    );

    let mut session = live_session(
        "codex",
        Some(AgentAccount {
            plan: Some("pro".to_owned()),
            metered: Some(true),
            ..Default::default()
        }),
        2_000,
    );
    session.context.as_mut().unwrap().agent_version = Some("0.160.0".to_owned());
    merge_live_accounts(&reporting(vec![session]), &runtime, &RoomLoginSet::native());
    let account = stored_account(&runtime, "codex");
    assert_eq!(
        (
            account.plan.as_deref(),
            account.metered,
            account.version.as_deref()
        ),
        (Some("pro"), Some(true), Some("0.160.0"))
    );
    assert_eq!(
        read_accounts_cache(&path).logins[&key("codex")]
            .stamps
            .plan_at_ms,
        Some(2_000)
    );
}

#[test]
fn a_probed_account_id_change_clears_the_facts_the_new_reading_lacks() {
    let previous = AccountsCache {
        logins: BTreeMap::from([(
            key("codex"),
            stamped(
                AgentAccount {
                    plan: Some("team".to_owned()),
                    metered: Some(true),
                    account_id: Some("first".to_owned()),
                    ..Default::default()
                },
                1_000,
            ),
        )]),
    };
    let switched = AgentAccount {
        account_id: Some("second".to_owned()),
        credentials_updated_at_ms: Some(2_000),
        ..Default::default()
    };
    let merged = probe_merge(&previous, "codex", AccountProbe::Found(switched), 3_000);
    let account = merged.logins[&key("codex")].account.clone().unwrap();
    assert_eq!(
        (account.account_id.as_deref(), account.plan, account.metered),
        (Some("second"), None, None)
    );
    assert_eq!(merged.logins[&key("codex")].stamps.floor_at_ms, Some(2_000));
}

fn probe_merge(
    previous: &AccountsCache,
    kind: &str,
    outcome: AccountProbe,
    probed_at_ms: u64,
) -> AccountsCache {
    merge_probe_results(
        previous,
        &BTreeSet::new(),
        probed_at_ms,
        [ProviderProbeResult {
            outcome,
            ..successful_probe(kind)
        }],
    )
}

fn shared_facts(
    runtime: &RuntimePaths,
    kind: &str,
) -> (Option<String>, Option<bool>, Option<String>) {
    let account = stored_account(runtime, kind);
    (account.plan, account.metered, account.account_id)
}

#[test]
fn an_agy_switch_keeps_the_old_sessions_sidecar_out_of_the_new_account() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let old_account = AgentAccount {
        account_id: Some("a@example.com".to_owned()),
        plan: Some("ultra".to_owned()),
        ..Default::default()
    };
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("antigravity"), stamped(old_account.clone(), 1_000))]),
    };
    let probed = AgentAccount {
        account_id: Some("b@example.com".to_owned()),
        credentials_updated_at_ms: Some(3_000),
        ..Default::default()
    };
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &probe_merge(&previous, "antigravity", AccountProbe::Found(probed), 4_000),
    );
    // The old session's sidecar still states account A, read before B's login.
    merge_live_accounts(
        &reporting(vec![live_session("antigravity", Some(old_account), 2_000)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert_eq!(
        shared_facts(&runtime, "antigravity"),
        (None, None, Some("b@example.com".to_owned()))
    );
}

#[test]
fn a_logout_keeps_the_pre_logout_sidecar_out_of_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let account = AgentAccount {
        plan: Some("team".to_owned()),
        metered: Some(true),
        account_id: Some("first".to_owned()),
        ..Default::default()
    };
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("codex"), stamped(account.clone(), 1_000))]),
    };
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &probe_merge(&previous, "codex", AccountProbe::LoggedOut, 5_000),
    );
    merge_live_accounts(
        &reporting(vec![live_session("codex", Some(account), 2_000)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    let record = &read_accounts_cache(&runtime.shared_accounts_path()).logins[&key("codex")];
    assert_eq!(record.login, Some(RecordedLogin::LoggedOut));
    assert_eq!(shared_facts(&runtime, "codex"), (None, None, None));
}

#[test]
fn a_login_after_a_logout_applies_a_credential_older_than_the_logout() {
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("antigravity"), stamped(AgentAccount::default(), 1_000))]),
    };
    let logged_out = probe_merge(&previous, "antigravity", AccountProbe::LoggedOut, 5_000);
    // A restored credential file keeps the mtime it had before the logout.
    let restored = AgentAccount {
        account_id: Some("a@example.com".to_owned()),
        metered: Some(true),
        credentials_updated_at_ms: Some(3_000),
        ..Default::default()
    };
    let merged = probe_merge(
        &logged_out,
        "antigravity",
        AccountProbe::Found(restored),
        6_000,
    );
    let account = merged.logins[&key("antigravity")].account.clone().unwrap();
    assert_eq!(
        (account.account_id.as_deref(), account.metered),
        (Some("a@example.com"), Some(true))
    );
}

#[test]
fn a_delegated_switch_to_a_planless_credential_drops_the_old_plan() {
    let anthropic = AgentAccount {
        scope: ProviderAccountScope::sub_provider("anthropic", "oauth"),
        sub_provider: Some("anthropic".to_owned()),
        plan: Some("max".to_owned()),
        metered: Some(true),
        ..Default::default()
    };
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("pi"), stamped(anthropic, 1_000))]),
    };
    let go = AgentAccount {
        scope: ProviderAccountScope::sub_provider("opencode", "oauth"),
        sub_provider: Some("opencode".to_owned()),
        metered: Some(true),
        credentials_updated_at_ms: Some(2_000),
        ..Default::default()
    };
    let merged = probe_merge(&previous, "pi", AccountProbe::Found(go), 3_000);
    let account = merged.logins[&key("pi")].account.clone().unwrap();
    assert_eq!(
        (
            account.sub_provider.as_deref(),
            account.plan,
            account.metered
        ),
        (Some("opencode"), None, Some(true))
    );
}

#[test]
fn a_live_session_on_another_account_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(
                key("codex"),
                stamped(
                    AgentAccount {
                        account_id: Some("b".to_owned()),
                        ..Default::default()
                    },
                    2_000,
                ),
            )]),
        },
    );
    let on_a = AgentAccount {
        account_id: Some("a".to_owned()),
        plan: Some("pro".to_owned()),
        metered: Some(true),
        ..Default::default()
    };
    merge_live_accounts(
        &reporting(vec![live_session("codex", Some(on_a), 3_000)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert_eq!(
        shared_facts(&runtime, "codex"),
        (None, None, Some("b".to_owned()))
    );
}

#[test]
fn a_live_plan_is_stamped_when_its_source_read_it_not_at_a_later_refresh() {
    use crate::agents::context::record::AgentContextRecord;
    let at = |ms: i64| Timestamp::from_millisecond(ms).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    // The app server reads Plus at t1.
    let mut sidecar = AgentContextRecord::new(
        "codex",
        "s",
        crate::agents::AgentContext::new("codex", at(1_000)),
    );
    let mut app_server = crate::agents::AgentContext::new("codex", at(1_000));
    app_server.account = Some(AgentAccount {
        plan: Some("plus".to_owned()),
        ..Default::default()
    });
    sidecar.apply_context_refresh("codex", "s", app_server, None, false);
    // A usage read stores Pro at t2.
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(
                key("codex"),
                stamped(
                    AgentAccount {
                        plan: Some("pro".to_owned()),
                        ..Default::default()
                    },
                    2_000,
                ),
            )]),
        },
    );
    // A transcript-only refresh at t3 moves the context, not the account.
    let mut transcript = crate::agents::AgentContext::new("codex", at(3_000));
    transcript.session_name = Some("renamed".to_owned());
    assert!(sidecar.merge_observed("codex", transcript, at(3_000)));
    let mut session = root_agent("codex", "s", None);
    session.context = Some(sidecar.context);
    merge_live_accounts(&reporting(vec![session]), &runtime, &RoomLoginSet::native());
    assert_eq!(
        stored_account(&runtime, "codex").plan.as_deref(),
        Some("pro")
    );
}

#[test]
fn a_usage_plan_from_another_owner_applies_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(
                key("codex"),
                stamped(
                    AgentAccount {
                        account_key: Some("owner-a".to_owned()),
                        plan: Some("plus".to_owned()),
                        ..Default::default()
                    },
                    1_000,
                ),
            )]),
        },
    );
    let other = AccountUsageIdentity {
        account_key: Some("owner-b".to_owned()),
        ..Default::default()
    };
    merge_usage_plan(&runtime, &key("codex"), &other, "pro", 2_000);
    assert_eq!(
        stored_account(&runtime, "codex").plan.as_deref(),
        Some("plus")
    );
    let same = AccountUsageIdentity {
        account_key: Some("owner-a".to_owned()),
        ..Default::default()
    };
    merge_usage_plan(&runtime, &key("codex"), &same, "pro", 2_000);
    assert_eq!(
        stored_account(&runtime, "codex").plan.as_deref(),
        Some("pro")
    );
}

#[test]
fn repeated_live_passes_over_unchanged_facts_write_the_record_once() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("codex"), record(1, true, None))]),
        },
    );
    let account = AgentAccount {
        plan: Some("pro".to_owned()),
        metered: Some(true),
        account_id: Some("acct".to_owned()),
        ..Default::default()
    };
    let inode = || {
        std::fs::metadata(runtime.shared_accounts_path())
            .unwrap()
            .ino()
    };
    merge_live_accounts(
        &reporting(vec![live_session("codex", Some(account.clone()), 2_000)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    let written = inode();
    // The same session state, observed again later.
    merge_live_accounts(
        &reporting(vec![live_session("codex", Some(account), 3_000)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert_eq!(inode(), written);
    assert_eq!(
        stored_account(&runtime, "codex").plan.as_deref(),
        Some("pro")
    );
}

#[test]
fn a_pi_login_shows_the_same_alternates_on_the_sidebar_and_the_accounts_row() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let credential = |provider: &str, plan: &str| AgentAccount {
        scope: ProviderAccountScope::sub_provider(provider, "oauth"),
        sub_provider: Some(provider.to_owned()),
        plan: Some(plan.to_owned()),
        metered: Some(true),
        ..Default::default()
    };
    let account = AgentAccount {
        credentials: BTreeMap::from([
            (
                "anthropic".to_owned(),
                credential("anthropic", "Anthropic OAuth"),
            ),
            ("opencode".to_owned(), credential("opencode", "Go OAuth")),
            ("openai".to_owned(), credential("openai", "OpenAI OAuth")),
        ]),
        ..credential("openai", "OpenAI OAuth")
    };
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("pi"), record(1, true, Some(account)))]),
        },
    );
    let session = |observed_ms: i64, provider: &str| {
        let mut state = live_session("pi", None, observed_ms);
        state.context.as_mut().unwrap().sub_provider = Some(provider.to_owned());
        state
    };
    let snapshot = carded(vec![session(2_000, "openai"), session(2_001, "anthropic")]);
    merge_live_accounts(&snapshot, &runtime, &RoomLoginSet::native());

    let blocks = |panel: &SidebarProviderPanel| {
        (
            panel.sub_provider.clone(),
            panel
                .alternates
                .iter()
                .map(|alternate| alternate.sub_provider.clone())
                .collect::<Vec<_>>(),
        )
    };
    let sidebar = snapshot
        .with_provider_aggregates(
            &Default::default(),
            &cached_accounts_for_snapshot(&runtime, &RoomLoginSet::native()),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::from([key("pi")]),
        )
        .providers;
    let row_account = read_provider_accounts(&runtime).logins[&key("pi")].display_account();
    let row = crate::sidebar::enrich::provider_panels_from_caches(
        &runtime,
        &RoomLoginSet::native(),
        &crate::config::MachineConfig::default(),
        row_account
            .map(|account| (key("pi"), account))
            .into_iter()
            .collect(),
        &Default::default(),
    );
    let pi = |panels: &[SidebarProviderPanel]| {
        blocks(panels.iter().find(|panel| panel.kind == "pi").unwrap())
    };
    let expected = (
        Some("openai".to_owned()),
        vec![Some("anthropic".to_owned())],
    );
    assert_eq!(pi(&sidebar), expected);
    assert_eq!(pi(&row), expected);
}

#[test]
fn delegated_usage_plan_only_fills_an_empty_plan_on_its_credential() {
    let anthropic = ProviderAccountScope::sub_provider("anthropic", "oauth");
    let openai = ProviderAccountScope::sub_provider("openai", "oauth");
    let pi = |top_plan: Option<&str>, credential_plan: Option<&str>| AgentAccount {
        scope: openai.clone(),
        plan: top_plan.map(str::to_owned),
        credentials: BTreeMap::from([(
            "anthropic".to_owned(),
            AgentAccount {
                scope: anthropic.clone(),
                plan: credential_plan.map(str::to_owned),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let mut actual = Vec::new();
    for (scope, stored) in [
        (&openai, pi(Some("OpenAI OAuth"), None)),
        (&openai, pi(None, None)),
        (&anthropic, pi(None, Some("Anthropic OAuth"))),
        (&anthropic, pi(None, None)),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = shared_runtime(&dir);
        write_accounts_cache(
            &runtime.shared_accounts_path(),
            &AccountsCache {
                logins: BTreeMap::from([(key("pi"), record(1, true, Some(stored)))]),
            },
        );
        let identity = AccountUsageIdentity {
            scope: scope.clone(),
            ..Default::default()
        };
        merge_usage_plan(&runtime, &key("pi"), &identity, "pro", 2);
        let account = stored_account(&runtime, "pi");
        actual.push((account.plan, account.credentials["anthropic"].plan.clone()));
    }
    let some = |plan: &str| Some(plan.to_owned());
    assert_eq!(
        actual,
        [
            (some("OpenAI OAuth"), None),
            (some("pro"), None),
            (None, some("Anthropic OAuth")),
            (None, some("pro")),
        ]
    );
}

#[test]
fn a_probe_landing_after_a_newer_live_write_keeps_the_live_plan() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let mut stale = fresh_cache(unix_now_ms());
    stale.logins.insert(
        key("codex"),
        record(
            0,
            true,
            Some(AgentAccount {
                plan: Some("old".to_owned()),
                ..Default::default()
            }),
        ),
    );
    write_accounts_cache(&runtime.shared_accounts_path(), &stale);
    let live_at_ms = unix_now_ms();
    produce_accounts_with(
        &empty_snapshot(),
        &runtime,
        &RoomLoginSet::native(),
        |_, runtime, due, _, _| {
            assert!(due.contains(&key("codex")));
            // A live reading lands while the batch probes.
            let live = AgentAccount {
                plan: Some("live-plan".to_owned()),
                ..Default::default()
            };
            merge_live_accounts(
                &reporting(vec![live_session("codex", Some(live), live_at_ms as i64)]),
                runtime,
                &RoomLoginSet::native(),
            );
            ProbeRun {
                probed_at_ms: live_at_ms - 1,
                results: vec![ProviderProbeResult {
                    outcome: AccountProbe::Found(AgentAccount {
                        plan: Some("probe-plan".to_owned()),
                        ..Default::default()
                    }),
                    ..successful_probe("codex")
                }],
            }
        },
    );
    assert_eq!(
        stored_account(&runtime, "codex").plan.as_deref(),
        Some("live-plan")
    );
}

fn with_windows(mut state: AgentState) -> AgentState {
    state.context.as_mut().unwrap().rate_limits = Some(crate::agents::AgentRateLimits {
        windows: vec![crate::agents::RateLimitWindow {
            used_percentage: Some(10),
            duration_mins: Some(300),
            ..Default::default()
        }],
    });
    state
}

#[test]
fn a_keyring_agy_login_switches_to_a_newer_status_lines_account() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    // A keyring install's probe states no account id.
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("antigravity"), record(1, true, None))]),
        },
    );
    let line = |email: &str, plan: &str| AgentAccount {
        account_id: Some(email.to_owned()),
        plan: Some(plan.to_owned()),
        ..Default::default()
    };
    let merge = |account: AgentAccount, observed_ms: i64| {
        merge_live_accounts(
            &reporting(vec![live_session(
                "antigravity",
                Some(account),
                observed_ms,
            )]),
            &runtime,
            &RoomLoginSet::native(),
        );
    };
    merge(line("a@example.com", "ultra"), 2_000);
    merge(line("b@example.com", "pro"), 3_000);
    let switched = (
        Some("pro".to_owned()),
        None,
        Some("b@example.com".to_owned()),
    );
    assert_eq!(shared_facts(&runtime, "antigravity"), switched);
    // A status line read before the switch describes the previous account.
    merge(line("a@example.com", "ultra"), 2_500);
    assert_eq!(shared_facts(&runtime, "antigravity"), switched);
}

#[test]
fn a_switch_probe_never_lowers_an_earlier_logouts_floor() {
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("antigravity"), stamped(AgentAccount::default(), 1_000))]),
    };
    let logged_out = probe_merge(&previous, "antigravity", AccountProbe::LoggedOut, 5_000);
    let probed = |email: &str, mtime_ms: u64| {
        AccountProbe::Found(AgentAccount {
            account_id: Some(email.to_owned()),
            credentials_updated_at_ms: Some(mtime_ms),
            ..Default::default()
        })
    };
    // Restored credential files keep mtimes older than the logout.
    let first = probe_merge(
        &logged_out,
        "antigravity",
        probed("a@example.com", 4_500),
        6_000,
    );
    let switched = probe_merge(&first, "antigravity", probed("b@example.com", 4_800), 7_000);
    let record = &switched.logins[&key("antigravity")];
    assert_eq!(
        record.account.as_ref().unwrap().account_id.as_deref(),
        Some("b@example.com")
    );
    assert_eq!(record.stamps.floor_at_ms, Some(5_000));
}

#[test]
fn a_stale_credential_file_leaves_a_newer_unmetered_fact() {
    let unmetered = AgentAccount {
        metered: Some(false),
        ..Default::default()
    };
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("claude"), stamped(unmetered, 3_000))]),
    };
    let merged = probe_merge(
        &previous,
        "claude",
        AccountProbe::Found(AgentAccount {
            metered: Some(true),
            credentials_updated_at_ms: Some(1_000),
            ..Default::default()
        }),
        4_000,
    );
    let account = merged.logins[&key("claude")].account.as_ref().unwrap();
    assert_eq!(account.metered, Some(false));
}

#[test]
fn an_api_key_probe_replaces_the_oauth_logins_display_facts() {
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("antigravity"), stamped(AgentAccount::default(), 1_000))]),
    };
    let oauth = probe_merge(
        &previous,
        "antigravity",
        AccountProbe::Found(AgentAccount {
            account_id: Some("a@example.com".to_owned()),
            metered: Some(true),
            ..Default::default()
        }),
        2_000,
    );
    let api_key = probe_merge(
        &oauth,
        "antigravity",
        AccountProbe::Found(AgentAccount {
            plan: Some("API key".to_owned()),
            metered: Some(false),
            ..Default::default()
        }),
        3_000,
    );
    let record = &api_key.logins[&key("antigravity")];
    let account = record.account.as_ref().unwrap();
    assert_eq!(record.login, Some(RecordedLogin::LoggedIn));
    assert_eq!(account.account_id, None);
    assert_eq!(account.plan.as_deref(), Some("API key"));
    assert_eq!(account.metered, Some(false));
}

#[test]
fn a_pre_logout_session_with_windows_infers_no_metering() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let account = AgentAccount {
        plan: Some("team".to_owned()),
        account_id: Some("first".to_owned()),
        ..Default::default()
    };
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("codex"), stamped(account.clone(), 1_000))]),
    };
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &probe_merge(&previous, "codex", AccountProbe::LoggedOut, 5_000),
    );
    merge_live_accounts(
        &reporting(vec![with_windows(live_session(
            "codex",
            Some(account),
            2_000,
        ))]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert_eq!(shared_facts(&runtime, "codex"), (None, None, None));
}

#[test]
fn a_session_on_another_account_with_windows_infers_no_metering() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let stored = AgentAccount {
        account_id: Some("b".to_owned()),
        ..Default::default()
    };
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("codex"), stamped(stored, 2_000))]),
        },
    );
    let other = AgentAccount {
        account_id: Some("a".to_owned()),
        ..Default::default()
    };
    merge_live_accounts(
        &reporting(vec![with_windows(live_session(
            "codex",
            Some(other),
            3_000,
        ))]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert_eq!(
        shared_facts(&runtime, "codex"),
        (None, None, Some("b".to_owned()))
    );
}

fn pi_on(provider: &str, observed_ms: i64) -> AgentState {
    let mut state = live_session("pi", None, observed_ms);
    state.context.as_mut().unwrap().sub_provider = Some(provider.to_owned());
    state
}

fn write_pi_on_openai(runtime: &RuntimePaths) {
    let credential = |provider: &str| AgentAccount {
        scope: ProviderAccountScope::sub_provider(provider, "oauth"),
        sub_provider: Some(provider.to_owned()),
        ..Default::default()
    };
    let account = AgentAccount {
        credentials: BTreeMap::from([
            ("anthropic".to_owned(), credential("anthropic")),
            ("openai".to_owned(), credential("openai")),
        ]),
        ..credential("openai")
    };
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("pi"), record(1, true, Some(account)))]),
        },
    );
}

/// A room whose every session has a card, as the heavy pass's base carries.
fn carded(sessions: Vec<AgentState>) -> SidebarSnapshot {
    let repo = Path::new("/repo");
    let rows = sessions
        .iter()
        .map(|session| {
            let mut row = activity_row(true, None, Timestamp::now(), repo);
            row.id = session.agent_id.as_str().to_owned();
            row
        })
        .collect();
    let mut snapshot = reporting(sessions);
    snapshot.worktree_groups = vec![worktree_group(repo, rows)].into();
    snapshot
}

fn pi_alternates(runtime: &RuntimePaths) -> Vec<String> {
    read_provider_accounts(runtime).logins[&key("pi")]
        .display_account()
        .unwrap()
        .credentials
        .into_keys()
        .collect()
}

#[test]
fn another_rooms_pass_keeps_a_credential_this_room_runs_on() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_pi_on_openai(&runtime);
    let room_a = carded(vec![pi_on("openai", 2_000), pi_on("anthropic", 2_001)]);
    let room_b = carded(vec![pi_on("openai", 2_002)]);
    for room in [&room_a, &room_b, &room_a, &room_b] {
        merge_live_accounts(room, &runtime, &RoomLoginSet::native());
        assert_eq!(pi_alternates(&runtime), ["anthropic"]);
    }
    // Room A's second pass came within a third of the TTL, so it wrote nothing.
    let seen = read_provider_accounts(&runtime).logins[&key("pi")].live_credentials["anthropic"];
    merge_live_accounts(&room_a, &runtime, &RoomLoginSet::native());
    assert_eq!(
        read_provider_accounts(&runtime).logins[&key("pi")].live_credentials["anthropic"],
        seen
    );
}

#[test]
fn an_ended_session_adds_no_alternate() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_pi_on_openai(&runtime);
    let mut ended = pi_on("anthropic", 2_001);
    ended.ended_at = Some(Timestamp::from_millisecond(2_500).unwrap());
    merge_live_accounts(
        &carded(vec![pi_on("openai", 2_000), ended]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert!(pi_alternates(&runtime).is_empty());
}

#[test]
fn a_stopped_agents_unended_row_adds_no_alternate() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_pi_on_openai(&runtime);
    let mut room = carded(vec![pi_on("openai", 2_000)]);
    let stopped = pi_on("anthropic", 2_001);
    room.agents = std::sync::Arc::new(vec![room.agents[0].clone(), stopped]);
    merge_live_accounts(&room, &runtime, &RoomLoginSet::native());
    assert!(pi_alternates(&runtime).is_empty());
}

#[test]
fn a_credential_no_room_has_seen_lately_is_no_alternate() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_pi_on_openai(&runtime);
    let path = runtime.shared_accounts_path();
    let mut cache = read_accounts_cache(&path);
    let hour_ago = unix_now_ms() - 3_600_000;
    cache.logins.get_mut(&key("pi")).unwrap().live_credentials =
        BTreeMap::from([("anthropic".to_owned(), hour_ago)]);
    write_accounts_cache(&path, &cache);
    assert!(pi_alternates(&runtime).is_empty());
    // An older build's list of names reads as never seen.
    let mut encoded = serde_json::to_value(&cache.logins[&key("pi")]).unwrap();
    encoded["live_credentials"] = serde_json::json!(["anthropic"]);
    let legacy: ProviderRecord = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        legacy.live_credentials,
        BTreeMap::from([("anthropic".to_owned(), 0)])
    );
}

#[test]
fn a_scoped_usage_read_before_the_floor_fills_no_credential_plan() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let anthropic = ProviderAccountScope::sub_provider("anthropic", "oauth");
    let account = AgentAccount {
        scope: ProviderAccountScope::sub_provider("openai", "oauth"),
        credentials: BTreeMap::from([(
            "anthropic".to_owned(),
            AgentAccount {
                scope: anthropic.clone(),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let mut stored = record(1, true, Some(account));
    stored.stamps.floor_at_ms = Some(5_000);
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("pi"), stored)]),
        },
    );
    let identity = AccountUsageIdentity {
        scope: anthropic,
        ..Default::default()
    };
    merge_usage_plan(&runtime, &key("pi"), &identity, "pro", 2_000);
    assert_eq!(
        stored_account(&runtime, "pi").credentials["anthropic"].plan,
        None
    );
}

#[test]
fn a_status_line_repeated_after_a_relogin_applies_its_plan() {
    use crate::agents::context::record::AgentContextRecord;
    let at = |ms: i64| Timestamp::from_millisecond(ms).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let line = |ms: i64| {
        let mut context = crate::agents::AgentContext::new("antigravity", at(ms));
        context.account = Some(AgentAccount {
            plan: Some("ultra".to_owned()),
            ..Default::default()
        });
        context
    };
    let mut sidecar = AgentContextRecord::new(
        "antigravity",
        "s",
        crate::agents::AgentContext::new("antigravity", at(1_000)),
    );
    sidecar.merge_observed("antigravity", line(1_000), at(1_000));
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("antigravity"), record(1, true, None))]),
    };
    let logged_out = probe_merge(&previous, "antigravity", AccountProbe::LoggedOut, 2_000);
    let relogin = probe_merge(
        &logged_out,
        "antigravity",
        AccountProbe::Found(AgentAccount::default()),
        3_000,
    );
    write_accounts_cache(&runtime.shared_accounts_path(), &relogin);
    // The pane outlived the logout and reports the same account again.
    sidecar.merge_observed("antigravity", line(4_000), at(4_000));
    let mut session = root_agent("antigravity", "s", None);
    session.context = Some(sidecar.context);
    merge_live_accounts(&reporting(vec![session]), &runtime, &RoomLoginSet::native());
    assert_eq!(
        stored_account(&runtime, "antigravity").plan.as_deref(),
        Some("ultra")
    );
}

#[test]
fn a_repeated_logout_keeps_the_first_logouts_floor() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let empty = AccountsCache {
        logins: BTreeMap::from([(key("codex"), record(1, true, None))]),
    };
    let first = probe_merge(&empty, "codex", AccountProbe::LoggedOut, 1_000);
    // A kind with no login check reads logged out at every probe.
    let second = probe_merge(&first, "codex", AccountProbe::LoggedOut, 3_000);
    assert_eq!(second.logins[&key("codex")].stamps.floor_at_ms, Some(1_000));
    write_accounts_cache(&runtime.shared_accounts_path(), &second);
    merge_live_accounts(
        &reporting(vec![versioned_session("codex-idle", "1.2.3", 2)]),
        &runtime,
        &RoomLoginSet::native(),
    );
    assert_eq!(
        stored_account(&runtime, "codex").version.as_deref(),
        Some("1.2.3")
    );
}

fn live_merge(runtime: &RuntimePaths, session: AgentState) {
    merge_live_accounts(&reporting(vec![session]), runtime, &RoomLoginSet::native());
}

fn probe_into(runtime: &RuntimePaths, kind: &str, outcome: AccountProbe, probed_at_ms: u64) {
    let path = runtime.shared_accounts_path();
    let merged = probe_merge(&read_accounts_cache(&path), kind, outcome, probed_at_ms);
    write_accounts_cache(&path, &merged);
}

fn keyring_agy(runtime: &RuntimePaths) {
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("antigravity"), record(1, true, None))]),
        },
    );
}

fn agy_id(id: &str) -> AgentAccount {
    AgentAccount {
        account_id: Some(id.to_owned()),
        ..Default::default()
    }
}

fn agy_probe(id: &str, mtime_ms: u64) -> AccountProbe {
    AccountProbe::Found(AgentAccount {
        credentials_updated_at_ms: Some(mtime_ms),
        ..agy_id(id)
    })
}

#[test]
fn a_probe_stating_the_live_id_holds_it_against_a_newer_session() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    keyring_agy(&runtime);
    live_merge(
        &runtime,
        live_session("antigravity", Some(agy_id("a")), 2_000),
    );
    // The credential predates the live stamp, yet the probe states that id.
    probe_into(&runtime, "antigravity", agy_probe("a", 1_000), 4_000);
    live_merge(
        &runtime,
        live_session("antigravity", Some(agy_id("b")), 5_000),
    );
    assert_eq!(
        stored_account(&runtime, "antigravity")
            .account_id
            .as_deref(),
        Some("a")
    );
}

#[test]
fn a_probe_returns_a_live_switch_to_its_own_id() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    keyring_agy(&runtime);
    live_merge(
        &runtime,
        live_session("antigravity", Some(agy_id("a")), 2_000),
    );
    live_merge(
        &runtime,
        live_session("antigravity", Some(agy_id("b")), 3_000),
    );
    assert_eq!(
        stored_account(&runtime, "antigravity")
            .account_id
            .as_deref(),
        Some("b")
    );
    probe_into(&runtime, "antigravity", agy_probe("a", 2_500), 4_000);
    let record = &read_accounts_cache(&runtime.shared_accounts_path()).logins[&key("antigravity")];
    assert_eq!(
        record.account.as_ref().unwrap().account_id.as_deref(),
        Some("a")
    );
    assert_eq!(record.stamps.floor_at_ms, Some(3_000));
}

#[test]
fn a_credential_read_is_owned_by_that_credentials_key() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    let go = ProviderAccountScope::sub_provider("opencode-go", "api");
    let account = AgentAccount {
        scope: ProviderAccountScope::sub_provider("anthropic", "oauth"),
        account_key: Some("K_a".to_owned()),
        credentials: BTreeMap::from([(
            "opencode-go".to_owned(),
            AgentAccount {
                scope: go.clone(),
                account_key: Some("K_go".to_owned()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("opencode"), record(1, true, Some(account)))]),
        },
    );
    let read = |owner: &str, plan: &str| {
        let identity = AccountUsageIdentity {
            scope: go.clone(),
            account_key: Some(owner.to_owned()),
            ..Default::default()
        };
        merge_usage_plan(&runtime, &key("opencode"), &identity, plan, 2_000);
        stored_account(&runtime, "opencode").credentials["opencode-go"]
            .plan
            .clone()
    };
    assert_eq!(read("K_x", "other"), None);
    assert_eq!(read("K_go", "go"), Some("go".to_owned()));
}

#[test]
fn a_probeless_kind_infers_metering_from_a_session_with_windows() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    // A kind without a login probe reads as logged out on every probe pass.
    probe_into(&runtime, "omp", AccountProbe::LoggedOut, 1_000);
    live_merge(&runtime, with_windows(live_session("omp", None, 2_000)));
    assert_eq!(stored_account(&runtime, "omp").metered, Some(true));
    probe_into(&runtime, "omp", AccountProbe::LoggedOut, 3_000);
    live_merge(&runtime, with_windows(live_session("omp", None, 4_000)));
    assert_eq!(stored_account(&runtime, "omp").metered, Some(true));
}

#[test]
fn a_keyring_switch_keeps_the_switching_sessions_own_windows() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    keyring_agy(&runtime);
    live_merge(
        &runtime,
        live_session("antigravity", Some(agy_id("a")), 1_000),
    );
    // B's first windows carry its process start, before its status line.
    let session = windows_observed_at(live_session("antigravity", Some(agy_id("b")), 3_000), 1_500);
    live_merge(&runtime, session);
    assert_eq!(
        stored_account(&runtime, "antigravity")
            .account_id
            .as_deref(),
        Some("b")
    );
    let floors = super::super::rate_limits::identity_floors(&runtime);
    assert_eq!(
        floors.get(&key("antigravity")),
        Some(&Timestamp::from_millisecond(1_500).unwrap())
    );
}

#[test]
fn a_live_switch_back_floors_the_replaced_accounts_windows() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    keyring_agy(&runtime);
    live_merge(
        &runtime,
        live_session("antigravity", Some(agy_id("a")), 1_000),
    );
    // A pane on B, started at 2000, switches the record to B at 2500.
    live_merge(
        &runtime,
        windows_observed_at(live_session("antigravity", Some(agy_id("b")), 2_500), 2_000),
    );
    // A pane on A that has run since 1500 switches back: B's windows since
    // 2500 must not pass as A's.
    live_merge(
        &runtime,
        windows_observed_at(live_session("antigravity", Some(agy_id("a")), 4_000), 1_500),
    );
    assert_eq!(
        stored_account(&runtime, "antigravity")
            .account_id
            .as_deref(),
        Some("a")
    );
    let floors = super::super::rate_limits::identity_floors(&runtime);
    assert_eq!(
        floors.get(&key("antigravity")),
        Some(&Timestamp::from_millisecond(4_000).unwrap())
    );
}

fn windows_observed_at(state: AgentState, observed_ms: i64) -> AgentState {
    let mut state = with_windows(state);
    let observed_at = Timestamp::from_millisecond(observed_ms).unwrap();
    for window in &mut state
        .context
        .as_mut()
        .unwrap()
        .rate_limits
        .as_mut()
        .unwrap()
        .windows
    {
        window.observed_at = Some(observed_at);
    }
    state
}

#[test]
fn an_empty_live_account_id_states_no_identity() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = shared_runtime(&dir);
    write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(key("codex"), stamped(agy_id("a"), 1_000))]),
        },
    );
    let reading = AgentAccount {
        plan: Some("pro".to_owned()),
        ..agy_id("")
    };
    live_merge(&runtime, live_session("codex", Some(reading), 2_000));
    assert_eq!(
        shared_facts(&runtime, "codex"),
        (Some("pro".to_owned()), None, Some("a".to_owned()))
    );
}

#[test]
fn a_first_logout_never_lowers_a_later_floor() {
    let mut stored = stamped(agy_id("a"), 1_000);
    stored.login = Some(RecordedLogin::LoggedIn);
    stored.stamps.floor_at_ms = Some(9_000);
    let previous = AccountsCache {
        logins: BTreeMap::from([(key("codex"), stored)]),
    };
    let merged = probe_merge(&previous, "codex", AccountProbe::LoggedOut, 5_000);
    assert_eq!(merged.logins[&key("codex")].stamps.floor_at_ms, Some(9_000));
}
