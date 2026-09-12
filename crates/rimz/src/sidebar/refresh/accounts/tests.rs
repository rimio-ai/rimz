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
    let merged = probe_accounts_with(&due, &cache, &BTreeSet::new(), now_ms, |kind, _active| {
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
            !due_provider_logins(&cache, &snapshot, &native_logins(), now_ms).contains(&key(kind))
        );

        cache.logins.get_mut(&key(kind)).unwrap().probed_at_ms =
            now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
        assert!(
            due_provider_logins(&cache, &snapshot, &native_logins(), now_ms).contains(&key(kind)),
            "an active {kind} account without a version re-probes after the retry window"
        );

        cache.logins.get_mut(&key(kind)).unwrap().ok = false;
        cache.logins.get_mut(&key(kind)).unwrap().probed_at_ms = now_ms;
        assert!(
            !due_provider_logins(&cache, &snapshot, &native_logins(), now_ms).contains(&key(kind)),
            "a failed {kind} probe waits for its own failure TTL"
        );
    }
}
