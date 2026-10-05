use std::collections::BTreeMap;

use jiff::Timestamp;

use crate::agents::account::{AccountsCache, ProviderRecord, read_rate_limits_cache};
use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow};
use crate::ids::WorkspaceId;
use crate::sidebar::refresh::credits::{CreditsCache, ProviderCreditsEntry};
use crate::sidebar::test_support::{provider_panel, snapshot_with_panels};

use super::*;

#[test]
fn published_usage_preserves_identity_and_only_writes_present_credits() {
    for with_credits in [true, false] {
        let (_dir, runtime) = account_usage_runtime();
        let key = LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"));
        let identity = crate::agents::AccountUsageIdentity {
            scope: crate::agents::ProviderAccountScope::sub_provider("alibaba", "international"),
            account_key: Some("opaque-fingerprint".to_owned()),
            credentials_stamp: None,
        };
        publish_account_usage_snapshot(
            &runtime,
            &key,
            identity.clone(),
            AccountUsageSnapshot {
                rate_limits: Some(AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(25),
                        duration_mins: Some(10_080),
                        ..Default::default()
                    }],
                }),
                reset_credits: with_credits.then_some(crate::agents::ResetCredits {
                    count: 1,
                    soonest_expiry: None,
                    expiries: Vec::new(),
                    effect: crate::agents::RedeemEffect::RestartsWindow,
                }),
                ..Default::default()
            },
        );

        let cache =
            crate::agents::account::read_rate_limits_cache(&runtime.shared_rate_limits_path());
        let entry = cache.entries.get(&key).unwrap();
        assert_eq!(entry.scope, identity.scope);
        assert_eq!(entry.account_key, identity.account_key);
        let cache = super::super::credits::read_credits_cache(&runtime.shared_credits_path());
        if with_credits {
            assert_eq!(cache.logins.get(&key).unwrap().scope, identity.scope);
        } else {
            assert!(!cache.logins.contains_key(&key));
        }
    }
}

fn complete_realtime() -> AccountUsageSnapshot {
    AccountUsageSnapshot {
        plan: Some("pro".to_owned()),
        extra_credits: Some(crate::agents::ExtraCredits::Disabled),
        rate_limits: Some(AgentRateLimits::default()),
        reset_credits: None,
    }
}

fn account_usage_runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn spare_room() -> (LoginKey, RoomLoginSet) {
    let accounts = toml::from_str("[codex.spare]\nhome = '/srv/spare'\n").unwrap();
    let catalog = crate::agents::LoginCatalog::from_config(&accounts).unwrap();
    let set = RoomLoginSet::new(Some(Default::default()), Some(catalog), BTreeMap::new());
    ("codex@spare".parse().unwrap(), set)
}

#[test]
fn claimed_usage_refuses_and_cancels_a_login_outside_the_catalog() {
    let (_dir, runtime) = account_usage_runtime();
    let (_, set) = spare_room();
    for refused in ["codex@gone", "claude@work"] {
        let refused: LoginKey = refused.parse().unwrap();
        let claim = claim_provider_account_usage(&runtime, &refused, None).unwrap();
        assert!(!refresh_claimed_account_usage_with(
            &runtime,
            &refused,
            claim,
            &set,
            |_, _, _| unreachable!("an undeclared login must not be probed"),
            |_, _| unreachable!("an undeclared login must not be probed"),
        ));
        assert!(!account_usage_claim_matches(&runtime, &refused, claim));
        assert!(claim_provider_account_usage(&runtime, &refused, None).is_some());
    }
}

#[test]
fn claimed_usage_reads_an_idle_account_by_the_direct_probe_alone() {
    let (_dir, runtime) = account_usage_runtime();
    let (spare, set) = spare_room();
    let claim = claim_provider_account_usage(&runtime, &spare, None).unwrap();
    let credits = crate::agents::ResetCredits {
        count: 2,
        soonest_expiry: None,
        expiries: Vec::new(),
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };
    assert!(refresh_claimed_account_usage_with(
        &runtime,
        &spare,
        claim,
        &set,
        |_, _, _| unreachable!("the room's app server speaks for the room's own login"),
        |_, env| {
            assert_eq!(
                env.get("CODEX_HOME").map(String::as_str),
                Some("/srv/spare")
            );
            crate::agents::AccountUsageProbe::Found {
                identity: Default::default(),
                snapshot: AccountUsageSnapshot {
                    rate_limits: Some(usage_windows(12)),
                    reset_credits: Some(credits.clone()),
                    ..Default::default()
                },
            }
        },
    ));
    let entry = super::super::credits::read_credits_cache(&runtime.shared_credits_path()).logins
        [&spare]
        .clone();
    assert_eq!(entry.reset_credits, Some(credits));
    assert_eq!(entry.direct_query_claim, None);
    assert_ne!(entry.oauth_read_at_ms, 0);
    assert_eq!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries[&spare]
            .limits
            .windows[0]
            .used_percentage,
        Some(12)
    );
}

#[test]
fn idle_accounts_are_claimed_on_their_own_floor_beside_the_in_use_cadence() {
    let (_dir, runtime) = account_usage_runtime();
    let (spare, set) = spare_room();
    let default_key: LoginKey = "codex@default".parse().unwrap();
    let hour_ms = 60 * 60 * 1_000;
    let attempted = |ago_ms: u64| ProviderCreditsEntry {
        oauth_read_at_ms: crate::utils::time::unix_now_ms() - ago_ms,
        ok: true,
        ..Default::default()
    };
    let snapshot = snapshot_with_panels(
        runtime.workspace_id.clone(),
        vec![provider_panel("codex", Vec::new())],
    );
    let spawned = |spare_entry: Option<ProviderCreditsEntry>| {
        let mut logins = BTreeMap::from([(default_key.clone(), attempted(hour_ms))]);
        logins.extend(spare_entry.map(|entry| (spare.clone(), entry)));
        super::super::credits::write_credits_cache(
            &runtime.shared_credits_path(),
            &CreditsCache {
                logins,
                ..Default::default()
            },
        );
        let mut spawned = Vec::new();
        refresh_account_usage_with(&snapshot, &runtime, &set, |_, key, _| {
            spawned.push(key.to_string());
            true
        });
        spawned
    };

    assert_eq!(spawned(None), ["codex@default", "codex@spare"]);
    assert_eq!(
        spawned(Some(attempted(hour_ms))),
        ["codex@default"],
        "an hour-old read is due for the in-use login alone"
    );
    assert_eq!(
        spawned(Some(attempted(4 * hour_ms))),
        ["codex@default", "codex@spare"]
    );
}

#[test]
fn claimed_usage_keeps_refreshing_a_live_agents_old_login() {
    let (_dir, runtime) = account_usage_runtime();
    let work: LoginKey = "claude@work".parse().unwrap();
    let accounts = toml::from_str("[claude.work]\nhome = '/srv/work'\n").unwrap();
    let catalog = crate::agents::LoginCatalog::from_config(&accounts).unwrap();
    let mut agent = crate::agents::AgentState::seed(
        work.kind.clone(),
        "root".into(),
        crate::agents::AgentStatus::Idle,
        Timestamp::UNIX_EPOCH,
    );
    agent.login = Some(work.name.clone());
    let set = RoomLoginSet::new(Some(Default::default()), Some(catalog), BTreeMap::new())
        .with_agents(&[agent]);
    let claim = claim_provider_account_usage(&runtime, &work, None).unwrap();
    assert!(refresh_claimed_account_usage_with(
        &runtime,
        &work,
        claim,
        &set,
        |adapter, runtime, env| adapter.probe_realtime_account_usage(runtime, env),
        |_, env| {
            assert_eq!(
                env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
                Some("/srv/work")
            );
            crate::agents::AccountUsageProbe::Found {
                identity: Default::default(),
                snapshot: AccountUsageSnapshot {
                    rate_limits: Some(usage_windows(12)),
                    ..Default::default()
                },
            }
        },
    ));
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache.entries[&work].limits.windows[0].used_percentage,
        Some(12)
    );
}

#[test]
fn forced_account_usage_refresh_invalidates_throttle_before_direct_claim() {
    let (_dir, runtime) = account_usage_runtime();
    super::super::credits::write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderCreditsEntry {
                    oauth_read_at_ms: 123,
                    auth_settled: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    let mut called = false;

    assert!(refresh_provider_usage_with(
        &runtime,
        &ProviderLogin::default_for(crate::ids::AgentKind::new_unchecked("claude")),
        true,
        |runtime, kind| {
            called = true;
            assert_eq!(kind.kind().as_str(), "claude");
            let cache = super::super::credits::read_credits_cache(&runtime.shared_credits_path());
            let entry = &cache.logins[&kind.key()];
            assert_eq!(entry.oauth_read_at_ms, 0);
            assert!(!entry.auth_settled);
            assert_eq!(entry.direct_query_claim, None);
            true
        }
    ));
    assert!(called);
}

fn usage_windows(percent: u8) -> AgentRateLimits {
    AgentRateLimits {
        windows: vec![RateLimitWindow {
            duration_mins: Some(300),
            used_percentage: Some(percent),
            source: crate::agents::context::WindowSource::Authoritative,
            ..Default::default()
        }],
    }
}

#[test]
fn account_usage_completion_publishes_complete_realtime_without_fallback() {
    let (_dir, runtime) = account_usage_runtime();
    let mut realtime = complete_realtime();
    realtime.rate_limits = Some(usage_windows(12));

    let wrote = complete_realtime_account_usage_with(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex")),
        realtime,
        |_, _| unreachable!("complete realtime usage needs no direct fallback"),
    );

    assert!(wrote);
    let credits = super::super::credits::read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(
        credits.logins
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"))]
            .plan
            .as_deref(),
        Some("pro")
    );
    assert_eq!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"))]
            .limits
            .windows[0]
            .used_percentage,
        Some(12)
    );
}

#[test]
fn account_usage_completion_combines_realtime_credits_with_direct_windows() {
    let (_dir, runtime) = account_usage_runtime();
    let realtime = AccountUsageSnapshot {
        plan: Some("pro".to_owned()),
        ..Default::default()
    };

    let wrote = complete_realtime_account_usage_with(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex")),
        realtime,
        |runtime, kind| {
            publish_account_usage_snapshot(
                runtime,
                kind,
                AccountUsageIdentity::default(),
                AccountUsageSnapshot {
                    rate_limits: Some(usage_windows(34)),
                    ..Default::default()
                },
            )
        },
    );

    assert!(wrote);
    let credits = super::super::credits::read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(
        credits.logins
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"))]
            .plan
            .as_deref(),
        Some("pro")
    );
    assert_eq!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"))]
            .limits
            .windows[0]
            .used_percentage,
        Some(34)
    );
}

#[test]
fn account_usage_completion_offline_skips_publication_and_probe() {
    if !crate::agents::credits::oauth_usage_offline() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sidebar::refresh::usage::tests::account_usage_completion_offline_skips_publication_and_probe",
                "--nocapture",
            ])
            .env("RIMZ_OAUTH_USAGE_OFFLINE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_dir, runtime) = account_usage_runtime();
    let called = std::cell::Cell::new(false);
    let wrote = complete_realtime_account_usage_with(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex")),
        complete_realtime(),
        |_, _| {
            called.set(true);
            true
        },
    );

    assert!(!wrote);
    assert!(!called.get());
    assert!(
        !super::super::credits::read_credits_cache(&runtime.shared_credits_path())
            .logins
            .contains_key(&crate::ids::LoginKey::default_for(
                crate::ids::AgentKind::new_unchecked("codex")
            ))
    );
}

#[test]
fn authoritative_direct_completion_survives_live_session_exit() {
    let (_dir, runtime) = account_usage_runtime();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let identity = AccountUsageIdentity {
        account_key: Some("claude-account".to_owned()),
        ..Default::default()
    };
    super::super::credits::write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderCreditsEntry {
                    account_key: identity.account_key.clone(),
                    ok: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    super::super::rate_limits::merge_account_rate_limits(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
        identity.clone(),
        AgentRateLimits {
            windows: vec![
                RateLimitWindow {
                    duration_mins: Some(300),
                    used_percentage: Some(0),
                    source: crate::agents::context::WindowSource::Authoritative,
                    ..Default::default()
                },
                RateLimitWindow {
                    duration_mins: Some(10_080),
                    used_percentage: Some(0),
                    source: crate::agents::context::WindowSource::Authoritative,
                    ..Default::default()
                },
            ],
        },
    );
    let claim = claim_provider_account_usage(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
        Some(identity.clone()),
    )
    .unwrap();
    assert!(complete_direct_account_usage(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
        claim,
        crate::agents::AccountUsageProbe::Found {
            identity,
            snapshot: AccountUsageSnapshot {
                rate_limits: Some(AgentRateLimits {
                    windows: vec![
                        RateLimitWindow {
                            duration_mins: Some(300),
                            used_percentage: Some(36),
                            resets_at: Some(future),
                            source: crate::agents::context::WindowSource::Authoritative,
                            ..Default::default()
                        },
                        RateLimitWindow {
                            duration_mins: Some(10_080),
                            used_percentage: Some(4),
                            resets_at: Some(future),
                            source: crate::agents::context::WindowSource::Authoritative,
                            ..Default::default()
                        },
                    ],
                }),
                ..Default::default()
            },
        },
    ));

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache.entries
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude"))]
            .limits
            .windows[0]
            .used_percentage,
        Some(36)
    );
    assert_eq!(
        cache.entries
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude"))]
            .limits
            .windows[1]
            .used_percentage,
        Some(4)
    );

    let mut idle = snapshot_with_panels(
        runtime.workspace_id.clone(),
        vec![provider_panel("claude", Vec::new())],
    );
    super::super::rate_limits::apply_cached_rate_limits(
        &mut idle,
        &runtime,
        &crate::agents::RoomLoginSet::native(),
    );
    assert_eq!(
        idle.providers[0]
            .windows
            .iter()
            .map(|window| (window.used_percentage, window.resets_at))
            .collect::<Vec<_>>(),
        [(Some(36), Some(future)), (Some(4), Some(future))]
    );
}

fn owned_usage_runtime(owner: &str) -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    super::super::credits::write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
                    "antigravity",
                )),
                ProviderCreditsEntry {
                    account_key: Some(owner.to_owned()),
                    plan: Some("old plan".to_owned()),
                    ok: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    super::super::rate_limits::merge_account_rate_limits(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("antigravity")),
        usage_identity(Some(owner)),
        AgentRateLimits {
            windows: vec![RateLimitWindow {
                duration_mins: Some(300),
                used_percentage: Some(88),
                source: crate::agents::context::WindowSource::Authoritative,
                ..Default::default()
            }],
        },
    );
    (dir, runtime)
}

fn usage_identity(owner: Option<&str>) -> AccountUsageIdentity {
    AccountUsageIdentity {
        account_key: owner.map(ToOwned::to_owned),
        ..Default::default()
    }
}

fn claim(runtime: &RuntimePaths) -> Uuid {
    claim_provider_account_usage(
        runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("antigravity")),
        None,
    )
    .unwrap()
}

fn windows(runtime: &RuntimePaths) -> Vec<RateLimitWindow> {
    read_rate_limits_cache(&runtime.shared_rate_limits_path())
        .entries
        .get(&crate::ids::LoginKey::default_for(
            crate::ids::AgentKind::new_unchecked("antigravity"),
        ))
        .map(|entry| entry.limits.windows.clone())
        .unwrap_or_default()
}

#[test]
fn direct_account_usage_completion_replaces_or_drops_windows_only_for_a_known_new_owner() {
    let (_dir, runtime) = owned_usage_runtime("owner-a");
    assert!(complete_direct_account_usage(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("antigravity")),
        claim(&runtime),
        crate::agents::AccountUsageProbe::Found {
            identity: usage_identity(Some("owner-b")),
            snapshot: AccountUsageSnapshot {
                plan: Some("new plan".to_owned()),
                rate_limits: Some(AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        duration_mins: Some(300),
                        used_percentage: Some(12),
                        source: crate::agents::context::WindowSource::Authoritative,
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            },
        },
    ));
    assert_eq!(windows(&runtime)[0].used_percentage, Some(12));
    assert_eq!(
        super::super::credits::read_credits_cache(&runtime.shared_credits_path()).logins
            [&"antigravity@default".parse().unwrap()]
            .account_key
            .as_deref(),
        Some("owner-b")
    );

    let (_dir, runtime) = owned_usage_runtime("owner-a");
    assert!(complete_direct_account_usage(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("antigravity")),
        claim(&runtime),
        crate::agents::AccountUsageProbe::Failed(usage_identity(Some("owner-b"))),
    ));
    assert!(windows(&runtime).is_empty());

    for failed_identity in [usage_identity(None), usage_identity(Some("owner-a"))] {
        let (_dir, runtime) = owned_usage_runtime("owner-a");
        assert!(complete_direct_account_usage(
            &runtime,
            &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("antigravity")),
            claim(&runtime),
            crate::agents::AccountUsageProbe::Failed(failed_identity),
        ));
        assert_eq!(windows(&runtime)[0].used_percentage, Some(88));
    }
}

#[test]
fn unknown_owner_source_reuses_same_scope_owner_until_ttl() {
    let (_dir, runtime) = owned_usage_runtime("owner-a");
    assert!(complete_direct_account_usage(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("antigravity")),
        claim(&runtime),
        crate::agents::AccountUsageProbe::Found {
            identity: usage_identity(Some("owner-a")),
            snapshot: AccountUsageSnapshot::default(),
        },
    ));

    assert_eq!(
        claim_provider_account_usage(
            &runtime,
            &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("antigravity")),
            None
        ),
        None
    );
}

#[test]
fn fresh_cached_account_usage_gates_helper_and_synchronous_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    super::super::accounts::write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderRecord {
                    login: None,
                    probed_at_ms: 1,
                    ok: true,
                    account: Some(AgentAccount {
                        metered: Some(true),
                        credentials_updated_at_ms: Some(7),
                        ..Default::default()
                    }),
                },
            )]),
        },
    );
    super::super::credits::write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderCreditsEntry {
                    oauth_read_at_ms: crate::utils::time::unix_now_ms(),
                    credentials_stamp: Some(7),
                    account_key: Some("owner".to_owned()),
                    ok: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    let snapshot = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    let mut spawn_attempts = 0;

    refresh_account_usage_with(&snapshot, &runtime, &RoomLoginSet::native(), |_, _, _| {
        spawn_attempts += 1;
        true
    });

    assert_eq!(spawn_attempts, 0);
    assert!(!merge_account_usage_if_due(
        &runtime,
        &ProviderLogin::default_for(crate::ids::AgentKind::new_unchecked("claude"))
    ));
    assert_eq!(
        super::super::credits::read_credits_cache(&runtime.shared_credits_path()).logins
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude"))]
            .direct_query_claim,
        None
    );
}

#[test]
fn account_usage_changed_cached_credentials_claim_once_without_rereading_owner() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    super::super::accounts::write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderRecord {
                    login: None,
                    probed_at_ms: 1,
                    ok: true,
                    account: Some(AgentAccount {
                        metered: Some(true),
                        credentials_updated_at_ms: Some(8),
                        ..Default::default()
                    }),
                },
            )]),
        },
    );
    super::super::credits::write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderCreditsEntry {
                    oauth_read_at_ms: crate::utils::time::unix_now_ms(),
                    credentials_stamp: Some(7),
                    account_key: Some("owner".to_owned()),
                    ok: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    let snapshot = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    let mut spawn_attempts = 0;

    refresh_account_usage_with(&snapshot, &runtime, &RoomLoginSet::native(), |_, _, _| {
        spawn_attempts += 1;
        true
    });

    assert_eq!(spawn_attempts, 1);
    let claim = super::super::credits::read_credits_cache(&runtime.shared_credits_path()).logins
        [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude"))]
        .direct_query_claim
        .clone()
        .unwrap();
    assert_eq!(claim.credentials_stamp, Some(8));
    assert_eq!(claim.preflight_account_key.as_deref(), Some("owner"));
}

#[test]
fn metered_adapter_without_usage_source_creates_no_claim_or_helper() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = snapshot_with_panels(workspace, vec![provider_panel("cursor", Vec::new())]);
    let mut spawn_attempts = 0;
    refresh_account_usage_with(&snapshot, &runtime, &RoomLoginSet::native(), |_, _, _| {
        spawn_attempts += 1;
        true
    });
    assert_eq!(spawn_attempts, 0);
    assert!(
        !super::super::credits::read_credits_cache(&runtime.shared_credits_path())
            .logins
            .contains_key(&"cursor@default".parse().unwrap())
    );
}

#[test]
fn failed_spawn_cancels_claim_for_immediate_retry() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    super::super::accounts::write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: std::collections::BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderRecord {
                    login: None,
                    probed_at_ms: 1,
                    ok: true,
                    account: Some(AgentAccount {
                        metered: Some(true),
                        credentials_updated_at_ms: Some(7),
                        ..Default::default()
                    }),
                },
            )]),
        },
    );
    let snapshot = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    let mut spawn_attempts = 0;
    refresh_account_usage_with(&snapshot, &runtime, &RoomLoginSet::native(), |_, _, _| {
        spawn_attempts += 1;
        false
    });
    assert_eq!(spawn_attempts, 1);
    assert_eq!(
        super::super::credits::read_credits_cache(&runtime.shared_credits_path()).logins
            [&crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude"))]
            .direct_query_claim,
        None
    );
    assert!(
        claim_provider_account_usage(
            &runtime,
            &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
            Some(AccountUsageIdentity {
                credentials_stamp: Some(7),
                ..Default::default()
            })
        )
        .is_some()
    );
}

#[test]
fn simultaneous_schedulers_spawn_once_per_provider_kind() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    super::super::accounts::write_accounts_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderRecord {
                    login: None,
                    probed_at_ms: 1,
                    ok: true,
                    account: Some(AgentAccount {
                        metered: Some(true),
                        ..Default::default()
                    }),
                },
            )]),
        },
    );
    let snapshot = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    let spawns = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                refresh_account_usage_with(
                    &snapshot,
                    &runtime,
                    &RoomLoginSet::native(),
                    |_, _, _| {
                        spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        true
                    },
                );
            });
        }
    });

    assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn account_usage_segments_fit_strictly_inside_the_renewed_lease() {
    let realtime_segment = crate::agents::runtime_control::MAX_REALTIME_ACCOUNT_USAGE_DURATION
        + crate::disk::lock::LOCK_TIMEOUT;
    let direct_segment =
        crate::agents::credits::OAUTH_HTTP_MAX_DURATION * 2 + crate::disk::lock::LOCK_TIMEOUT;

    assert!(realtime_segment < crate::sidebar::timing::ACCOUNT_USAGE_CLAIM_TTL);
    assert!(direct_segment < crate::sidebar::timing::ACCOUNT_USAGE_CLAIM_TTL);
}
