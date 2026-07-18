use std::collections::BTreeMap;

use jiff::Timestamp;

use crate::agents::account::read_rate_limits_cache;
use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow};
use crate::ids::WorkspaceId;
use crate::sidebar::refresh::accounts::{AccountsCache, ProviderRecord};
use crate::sidebar::refresh::credits::{CreditsCache, ProviderCreditsEntry};
use crate::sidebar::test_support::{provider_panel, snapshot_with_panels};

use super::*;

fn complete_realtime() -> AccountUsageSnapshot {
    AccountUsageSnapshot {
        plan: Some("pro".to_owned()),
        extra_credits: Some(crate::agents::ExtraCredits::Disabled),
        rate_limits: Some(AgentRateLimits::default()),
        reset_credits: None,
    }
}

#[test]
fn account_usage_completion_decision_matrix() {
    let complete = complete_realtime();
    assert_eq!(
        account_usage_completion_decision(false, Some(&complete)),
        AccountUsageCompletionDecision::default()
    );
    assert_eq!(
        account_usage_completion_decision(true, None),
        AccountUsageCompletionDecision {
            publish_realtime: false,
            run_direct: true,
            merge_direct_windows: true,
        }
    );
    assert_eq!(
        account_usage_completion_decision(true, Some(&complete)),
        AccountUsageCompletionDecision {
            publish_realtime: true,
            run_direct: false,
            merge_direct_windows: false,
        }
    );

    let mut missing_plan = complete.clone();
    missing_plan.plan = None;
    assert_eq!(
        account_usage_completion_decision(true, Some(&missing_plan)),
        AccountUsageCompletionDecision {
            publish_realtime: true,
            run_direct: true,
            merge_direct_windows: false,
        }
    );

    let mut missing_extra = complete.clone();
    missing_extra.extra_credits = None;
    assert_eq!(
        account_usage_completion_decision(true, Some(&missing_extra)),
        AccountUsageCompletionDecision {
            publish_realtime: true,
            run_direct: true,
            merge_direct_windows: false,
        }
    );

    let mut missing_windows = complete.clone();
    missing_windows.rate_limits = None;
    assert_eq!(
        account_usage_completion_decision(true, Some(&missing_windows)),
        AccountUsageCompletionDecision {
            publish_realtime: true,
            run_direct: true,
            merge_direct_windows: true,
        }
    );

    let reset_only = AccountUsageSnapshot {
        reset_credits: Some(crate::agents::ResetCredits {
            count: 1,
            soonest_expiry: None,
            expiries: Vec::new(),
        }),
        ..Default::default()
    };
    assert_eq!(
        account_usage_completion_decision(true, Some(&reset_only)),
        AccountUsageCompletionDecision {
            publish_realtime: true,
            run_direct: true,
            merge_direct_windows: true,
        }
    );
}

fn account_usage_runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

#[test]
fn forced_account_usage_refresh_invalidates_throttle_before_direct_claim() {
    let (_dir, runtime) = account_usage_runtime();
    super::super::credits::write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "claude".to_owned(),
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

    assert!(refresh_account_usage_now_with(
        &runtime,
        "claude",
        |runtime, kind, merge_windows| {
            called = true;
            assert_eq!(kind, "claude");
            assert!(merge_windows);
            let cache = super::super::credits::read_credits_cache(&runtime.shared_credits_path());
            let entry = &cache.entries[kind];
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

    let wrote =
        complete_realtime_account_usage_with(&runtime, "codex", true, Some(realtime), |_, _, _| {
            unreachable!("complete realtime usage needs no direct fallback")
        });

    assert!(wrote);
    let credits = super::super::credits::read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(credits.entries["codex"].plan.as_deref(), Some("pro"));
    assert_eq!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries["codex"]
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
        "codex",
        true,
        Some(realtime),
        |runtime, kind, merge_windows| {
            assert!(merge_windows);
            publish_account_usage_snapshot(
                runtime,
                kind,
                ProviderAccountScope::KindWide,
                AccountUsageSnapshot {
                    rate_limits: Some(usage_windows(34)),
                    ..Default::default()
                },
                merge_windows,
            )
        },
    );

    assert!(wrote);
    let credits = super::super::credits::read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(credits.entries["codex"].plan.as_deref(), Some("pro"));
    assert_eq!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries["codex"]
            .limits
            .windows[0]
            .used_percentage,
        Some(34)
    );
}

#[test]
fn account_usage_completion_offline_skips_publication_and_probe() {
    let (_dir, runtime) = account_usage_runtime();
    let called = std::cell::Cell::new(false);
    let wrote = complete_realtime_account_usage_with(
        &runtime,
        "codex",
        false,
        Some(complete_realtime()),
        |_, _, _| {
            called.set(true);
            true
        },
    );

    assert!(!wrote);
    assert!(!called.get());
    assert!(
        !super::super::credits::read_credits_cache(&runtime.shared_credits_path())
            .entries
            .contains_key("codex")
    );
}

#[test]
fn fresh_realtime_claude_windows_defer_direct_window_publication() {
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/usage-refresh")),
        vec![crate::testkit::agent_state(
            "claude",
            "one",
            Timestamp::now(),
        )],
        Timestamp::now(),
    );
    snapshot.agents[0].context = Some(crate::agents::AgentContext::new("claude", snapshot.now));
    snapshot.agents[0].context.as_mut().unwrap().rate_limits = Some(AgentRateLimits {
        windows: vec![RateLimitWindow {
            duration_mins: Some(300),
            used_percentage: Some(12),
            resets_at: snapshot
                .now
                .checked_add(jiff::SignedDuration::from_hours(1))
                .ok(),
            ..Default::default()
        }],
    });
    assert!(!merge_windows_hint(&snapshot, "claude"));
    assert!(merge_windows_hint(&snapshot, "codex"));
}

#[test]
fn realtime_window_fallback_preserves_explicit_statusline_defer() {
    assert!(!direct_windows_should_publish(false, None, true));
    assert!(direct_windows_should_publish(false, None, false));
    assert!(direct_windows_should_publish(
        false,
        Some(&AccountUsageSnapshot::default()),
        true,
    ));
    assert!(!direct_windows_should_publish(
        false,
        Some(&AccountUsageSnapshot {
            rate_limits: Some(AgentRateLimits::default()),
            ..Default::default()
        }),
        true,
    ));
}

fn owned_usage_runtime(owner: &str) -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    super::super::credits::write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "antigravity".to_owned(),
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
    super::super::merge_account_rate_limits(
        &runtime,
        "antigravity",
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
    claim_provider_account_usage(runtime, "antigravity", None).unwrap()
}

fn windows(runtime: &RuntimePaths) -> Vec<RateLimitWindow> {
    read_rate_limits_cache(&runtime.shared_rate_limits_path())
        .entries
        .get("antigravity")
        .map(|entry| entry.limits.windows.clone())
        .unwrap_or_default()
}

#[test]
fn direct_account_usage_completion_replaces_or_drops_windows_only_for_a_known_new_owner() {
    let (_dir, runtime) = owned_usage_runtime("owner-a");
    assert!(complete_direct_account_usage(
        &runtime,
        "antigravity",
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
        true,
    ));
    assert_eq!(windows(&runtime)[0].used_percentage, Some(12));
    assert_eq!(
        super::super::credits::read_credits_cache(&runtime.shared_credits_path()).entries
            ["antigravity"]
            .account_key
            .as_deref(),
        Some("owner-b")
    );

    let (_dir, runtime) = owned_usage_runtime("owner-a");
    assert!(complete_direct_account_usage(
        &runtime,
        "antigravity",
        claim(&runtime),
        crate::agents::AccountUsageProbe::Failed(usage_identity(Some("owner-b"))),
        true,
    ));
    assert!(windows(&runtime).is_empty());

    for failed_identity in [usage_identity(None), usage_identity(Some("owner-a"))] {
        let (_dir, runtime) = owned_usage_runtime("owner-a");
        assert!(complete_direct_account_usage(
            &runtime,
            "antigravity",
            claim(&runtime),
            crate::agents::AccountUsageProbe::Failed(failed_identity),
            true,
        ));
        assert_eq!(windows(&runtime)[0].used_percentage, Some(88));
    }
}

#[test]
fn unknown_owner_source_reuses_same_scope_owner_until_ttl() {
    let (_dir, runtime) = owned_usage_runtime("owner-a");
    assert!(complete_direct_account_usage(
        &runtime,
        "antigravity",
        claim(&runtime),
        crate::agents::AccountUsageProbe::Found {
            identity: usage_identity(Some("owner-a")),
            snapshot: AccountUsageSnapshot::default(),
        },
        true,
    ));

    assert_eq!(
        claim_provider_account_usage(&runtime, "antigravity", None),
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
            providers: BTreeMap::from([(
                "claude".to_owned(),
                ProviderRecord {
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
            entries: BTreeMap::from([(
                "claude".to_owned(),
                ProviderCreditsEntry {
                    oauth_read_at_ms: crate::sidebar::timing::unix_now_ms(),
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

    refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
        spawn_attempts += 1;
        true
    });

    assert_eq!(spawn_attempts, 0);
    assert!(!merge_account_usage_if_due(&runtime, "claude", true));
    assert_eq!(
        super::super::credits::read_credits_cache(&runtime.shared_credits_path()).entries["claude"]
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
            providers: BTreeMap::from([(
                "claude".to_owned(),
                ProviderRecord {
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
            entries: BTreeMap::from([(
                "claude".to_owned(),
                ProviderCreditsEntry {
                    oauth_read_at_ms: crate::sidebar::timing::unix_now_ms(),
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

    refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
        spawn_attempts += 1;
        true
    });

    assert_eq!(spawn_attempts, 1);
    let claim =
        super::super::credits::read_credits_cache(&runtime.shared_credits_path()).entries["claude"]
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
    refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
        spawn_attempts += 1;
        true
    });
    assert_eq!(spawn_attempts, 0);
    assert!(
        !super::super::credits::read_credits_cache(&runtime.shared_credits_path())
            .entries
            .contains_key("cursor")
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
            providers: std::collections::BTreeMap::from([(
                "claude".to_owned(),
                ProviderRecord {
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
    refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
        spawn_attempts += 1;
        false
    });
    assert_eq!(spawn_attempts, 1);
    assert_eq!(
        super::super::credits::read_credits_cache(&runtime.shared_credits_path()).entries["claude"]
            .direct_query_claim,
        None
    );
    assert!(
        claim_provider_account_usage(
            &runtime,
            "claude",
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
            providers: BTreeMap::from([(
                "claude".to_owned(),
                ProviderRecord {
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
                refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
                    spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    true
                });
            });
        }
    });

    assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn account_usage_segments_fit_strictly_inside_the_renewed_lease() {
    let realtime_segment = crate::agents::codex::app_server::MAX_REALTIME_ACCOUNT_USAGE_DURATION
        + crate::store::lock::LOCK_TIMEOUT;
    let direct_segment =
        crate::agents::credits::OAUTH_HTTP_MAX_DURATION * 2 + crate::store::lock::LOCK_TIMEOUT;

    assert!(realtime_segment < crate::sidebar::timing::ACCOUNT_USAGE_CLAIM_TTL);
    assert!(direct_segment < crate::sidebar::timing::ACCOUNT_USAGE_CLAIM_TTL);
}
