use super::*;
use crate::agents::SpendTally;
use crate::ids::WorkspaceId;
use crate::{SidebarProviderPanel, SpendWindow};

fn merge_provider_credits_entry_if_due(
    runtime: &crate::RuntimePaths,
    kind: &str,
    current_stamp: Option<u64>,
    current_account_key: Option<String>,
    entry: impl FnOnce() -> ProviderCreditsEntry,
) -> Option<ProviderCreditsEntry> {
    super::merge_provider_credits_entry_if_due(
        runtime,
        kind,
        current_stamp,
        current_account_key,
        Default::default(),
        entry,
    )
}

fn panel(kind: &str, metered: bool) -> SidebarProviderPanel {
    SidebarProviderPanel {
        kind: kind.to_owned(),
        account_scope: Default::default(),
        product_name: kind.to_owned(),
        art: Vec::new(),
        color: 1,
        color_rgb: None,
        color_role: None,
        version: None,
        plan: None,
        metered,
        remote_control: Default::default(),
        spending: Some(SpendTally {
            month: SpendWindow {
                usd: 12.5,
                ..Default::default()
            },
            ..Default::default()
        }),
        day_budget: None,
        extra_credits: None,
        reset_credits: None,
        windows: Vec::new(),
    }
}

#[test]
fn scoped_plan_and_credits_require_a_matching_provider_panel() {
    let now_ms = unix_now_ms();
    let international =
        crate::agents::ProviderAccountScope::sub_provider("alibaba", "international");
    let cache = CreditsCache {
        refreshed_at_ms: now_ms,
        entries: BTreeMap::from([(
            "qwen".to_owned(),
            ProviderCreditsEntry {
                scope: international.clone(),
                observed_at_ms: now_ms,
                oauth_read_at_ms: now_ms,
                auth_settled: false,
                credentials_stamp: None,
                account_key: Some("alibaba-intl".to_owned()),
                plan: Some("Pro".to_owned()),
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
                reset_credits: None,
            },
        )]),
    };
    let workspace = crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp"));

    let mut mismatch = crate::sidebar::test_support::snapshot_with_panels(
        workspace.clone(),
        vec![panel("qwen", true)],
    );
    mismatch.providers[0].account_scope =
        crate::agents::ProviderAccountScope::sub_provider("alibaba", "china");
    apply_credits_cache_with(
        &mut mismatch,
        &cache,
        &crate::config::AccountsConfig::default(),
        now_ms,
    );
    assert!(mismatch.providers[0].plan.is_none());
    assert!(mismatch.providers[0].extra_credits.is_none());

    let mut matching =
        crate::sidebar::test_support::snapshot_with_panels(workspace, vec![panel("qwen", true)]);
    matching.providers[0].account_scope = international;
    apply_credits_cache_with(
        &mut matching,
        &cache,
        &crate::config::AccountsConfig::default(),
        now_ms,
    );
    assert_eq!(matching.providers[0].plan.as_deref(), Some("Pro"));
    assert_eq!(
        matching.providers[0]
            .extra_credits
            .as_ref()
            .and_then(ExtraCredits::remaining_usd),
        Some(5.0)
    );
}

#[test]
fn oauth_read_stamp_throttles_oauth_probe() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_credits_path();
    let now = unix_now_ms();
    write_credits_cache(
        &path,
        &CreditsCache {
            refreshed_at_ms: now,
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    scope: Default::default(),
                    observed_at_ms: 10,
                    oauth_read_at_ms: now,
                    auth_settled: false,
                    credentials_stamp: None,
                    account_key: None,
                    plan: None,
                    ok: true,
                    extra_credits: Some(ExtraCredits::known(None, Some(1.0), None)),
                    reset_credits: None,
                },
            )]),
        },
    );

    let mut calls = 0;
    assert!(
        merge_provider_credits_entry_if_due(&runtime, "codex", None, None, || {
            calls += 1;
            ProviderCreditsEntry {
                scope: Default::default(),
                observed_at_ms: unix_now_ms(),
                oauth_read_at_ms: unix_now_ms(),
                auth_settled: false,
                credentials_stamp: None,
                account_key: None,
                plan: None,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                reset_credits: None,
            }
        })
        .is_none()
    );
    assert_eq!(calls, 0, "fresh OAuth attempt stamp skips the fetch");

    let stale = unix_now_ms() - OAUTH_USAGE_TTL.as_millis() as u64 - 1;
    let mut cache = read_credits_cache(&path);
    cache.entries.get_mut("codex").unwrap().oauth_read_at_ms = stale;
    write_credits_cache(&path, &cache);

    let mut calls = 0;
    assert!(
        merge_provider_credits_entry_if_due(&runtime, "codex", None, None, || {
            calls += 1;
            ProviderCreditsEntry {
                scope: Default::default(),
                observed_at_ms: unix_now_ms(),
                oauth_read_at_ms: unix_now_ms(),
                auth_settled: false,
                credentials_stamp: None,
                account_key: None,
                plan: None,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                reset_credits: None,
            }
        })
        .is_some()
    );
    assert_eq!(calls, 1, "stale OAuth attempt stamp allows the fetch");
}

#[test]
fn settled_oauth_read_uses_long_ttl_and_credential_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_credits_path();
    let now = unix_now_ms();
    write_credits_cache(
        &path,
        &CreditsCache {
            refreshed_at_ms: now,
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    scope: Default::default(),
                    observed_at_ms: 10,
                    oauth_read_at_ms: now,
                    auth_settled: true,
                    credentials_stamp: Some(41),
                    account_key: None,
                    plan: None,
                    ok: false,
                    extra_credits: None,
                    reset_credits: None,
                },
            )]),
        },
    );

    let mut calls = 0;
    assert!(
        merge_provider_credits_entry_if_due(&runtime, "codex", Some(41), None, || {
            calls += 1;
            ProviderCreditsEntry {
                scope: Default::default(),
                observed_at_ms: unix_now_ms(),
                oauth_read_at_ms: unix_now_ms(),
                auth_settled: false,
                credentials_stamp: None,
                account_key: None,
                plan: None,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                reset_credits: None,
            }
        })
        .is_none(),
        "unchanged credentials keep a settled auth failure fresh on the long TTL"
    );
    assert_eq!(calls, 0);

    assert!(
        merge_provider_credits_entry_if_due(&runtime, "codex", Some(42), None, || {
            calls += 1;
            ProviderCreditsEntry {
                scope: Default::default(),
                observed_at_ms: unix_now_ms(),
                oauth_read_at_ms: unix_now_ms(),
                auth_settled: false,
                credentials_stamp: None,
                account_key: None,
                plan: None,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                reset_credits: None,
            }
        })
        .is_some(),
        "credential stamp change retries immediately"
    );
    assert_eq!(calls, 1);

    let old = unix_now_ms() - OAUTH_USAGE_SETTLED_TTL.as_millis() as u64 - 1;
    let mut cache = read_credits_cache(&path);
    let entry = cache.entries.get_mut("codex").unwrap();
    entry.oauth_read_at_ms = old;
    entry.auth_settled = true;
    entry.credentials_stamp = Some(42);
    write_credits_cache(&path, &cache);

    assert!(
        merge_provider_credits_entry_if_due(&runtime, "codex", Some(42), None, || {
            calls += 1;
            ProviderCreditsEntry {
                scope: Default::default(),
                observed_at_ms: unix_now_ms(),
                oauth_read_at_ms: unix_now_ms(),
                auth_settled: false,
                credentials_stamp: None,
                account_key: None,
                plan: None,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(3.0), None)),
                reset_credits: None,
            }
        })
        .is_some(),
        "settled auth failures are due after the long TTL"
    );
    assert_eq!(calls, 2);
}

#[test]
fn account_key_change_bypasses_fresh_oauth_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_credits_path();
    let now = unix_now_ms();
    write_credits_cache(
        &path,
        &CreditsCache {
            refreshed_at_ms: now,
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    scope: Default::default(),
                    observed_at_ms: 10,
                    oauth_read_at_ms: now,
                    auth_settled: false,
                    credentials_stamp: None,
                    account_key: Some("old".to_owned()),
                    plan: Some("plus".to_owned()),
                    ok: true,
                    extra_credits: Some(ExtraCredits::known(None, Some(1.0), None)),
                    reset_credits: None,
                },
            )]),
        },
    );

    let mut calls = 0;
    assert!(
        merge_provider_credits_entry_if_due(
            &runtime,
            "codex",
            None,
            Some("new".to_owned()),
            || {
                calls += 1;
                ProviderCreditsEntry {
                    scope: Default::default(),
                    observed_at_ms: unix_now_ms(),
                    oauth_read_at_ms: unix_now_ms(),
                    auth_settled: false,
                    credentials_stamp: None,
                    account_key: None,
                    plan: Some("pro".to_owned()),
                    ok: true,
                    extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                    reset_credits: None,
                }
            },
        )
        .is_some()
    );
    assert_eq!(calls, 1);
    assert_eq!(
        read_credits_cache(&path)
            .entries
            .get("codex")
            .and_then(|entry| entry.account_key.as_deref()),
        Some("new")
    );
}

#[test]
fn identified_account_bypasses_fresh_unowned_entry() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_credits_path();
    let now = unix_now_ms();
    write_credits_cache(
        &path,
        &CreditsCache {
            refreshed_at_ms: now,
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    scope: Default::default(),
                    observed_at_ms: 10,
                    oauth_read_at_ms: now,
                    auth_settled: false,
                    credentials_stamp: None,
                    account_key: None,
                    plan: Some("plus".to_owned()),
                    ok: true,
                    extra_credits: Some(ExtraCredits::known(None, Some(1.0), None)),
                    reset_credits: None,
                },
            )]),
        },
    );

    let mut calls = 0;
    assert!(
        merge_provider_credits_entry_if_due(
            &runtime,
            "codex",
            None,
            Some("current".to_owned()),
            || {
                calls += 1;
                ProviderCreditsEntry {
                    scope: Default::default(),
                    observed_at_ms: unix_now_ms(),
                    oauth_read_at_ms: unix_now_ms(),
                    auth_settled: false,
                    credentials_stamp: None,
                    account_key: None,
                    plan: Some("pro".to_owned()),
                    ok: true,
                    extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                    reset_credits: None,
                }
            },
        )
        .is_some()
    );
    assert_eq!(calls, 1);
}

#[test]
fn failed_oauth_after_account_key_change_does_not_carry_prior_account() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let reset_credits = ResetCredits {
        count: 4,
        soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
    };
    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 42,
            oauth_read_at_ms: 1,
            auth_settled: false,
            credentials_stamp: None,
            account_key: Some("old".to_owned()),
            plan: Some("plus".to_owned()),
            ok: true,
            extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
            reset_credits: Some(reset_credits),
        },
    );

    merge_provider_credits_entry_if_due(&runtime, "codex", None, Some("new".to_owned()), || {
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: unix_now_ms(),
            oauth_read_at_ms: unix_now_ms(),
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: false,
            extra_credits: None,
            reset_credits: None,
        }
    });

    let entry = read_credits_cache(&runtime.shared_credits_path())
        .entries
        .remove("codex")
        .expect("codex entry");
    assert!(!entry.ok);
    assert_eq!(entry.account_key.as_deref(), Some("new"));
    assert_eq!(entry.extra_credits, None);
    assert_eq!(entry.reset_credits, None);
    assert_eq!(entry.plan, None);
}

#[test]
fn failed_oauth_with_identified_account_clears_unowned_prior_display() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 42,
            oauth_read_at_ms: 1,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: Some("plus".to_owned()),
            ok: true,
            extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
            reset_credits: None,
        },
    );

    merge_provider_credits_entry_if_due(
        &runtime,
        "codex",
        None,
        Some("current".to_owned()),
        || ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: unix_now_ms(),
            oauth_read_at_ms: unix_now_ms(),
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: false,
            extra_credits: None,
            reset_credits: None,
        },
    );

    let entry = read_credits_cache(&runtime.shared_credits_path())
        .entries
        .remove("codex")
        .expect("codex entry");
    assert!(!entry.ok);
    assert_eq!(entry.account_key.as_deref(), Some("current"));
    assert_eq!(entry.plan, None);
    assert_eq!(entry.extra_credits, None);
}

#[test]
fn account_key_mismatch_is_symmetric() {
    assert!(!account_key_mismatch(None, None));
    assert!(!account_key_mismatch(Some("a"), Some("a")));
    assert!(account_key_mismatch(None, Some("a")));
    assert!(account_key_mismatch(Some("a"), None));
    assert!(account_key_mismatch(Some("a"), Some("b")));
}

#[test]
fn successful_observation_overrides_stale_preflight_owner() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            observed_at_ms: 1,
            oauth_read_at_ms: 1,
            account_key: Some("old".to_owned()),
            plan: Some("plus".to_owned()),
            ok: true,
            extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
            ..ProviderCreditsEntry::default()
        },
    );

    merge_provider_credits_entry_if_due(
        &runtime,
        "codex",
        None,
        Some("stale-preflight".to_owned()),
        || ProviderCreditsEntry {
            observed_at_ms: unix_now_ms(),
            oauth_read_at_ms: unix_now_ms(),
            account_key: Some("observed".to_owned()),
            plan: Some("pro".to_owned()),
            ok: true,
            ..ProviderCreditsEntry::default()
        },
    );

    let cache = read_credits_cache(&runtime.shared_credits_path());
    let entry = cache.entries.get("codex").expect("codex entry");
    assert_eq!(entry.account_key.as_deref(), Some("observed"));
    assert_eq!(entry.plan.as_deref(), Some("pro"));
    assert_eq!(entry.extra_credits, None);
}

#[test]
fn oauth_merge_records_settled_auth_and_success_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();

    merge_provider_credits_entry_if_due(&runtime, "codex", Some(7), None, || {
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: unix_now_ms(),
            oauth_read_at_ms: unix_now_ms(),
            auth_settled: true,
            credentials_stamp: Some(7),
            account_key: None,
            plan: None,
            ok: false,
            extra_credits: None,
            reset_credits: None,
        }
    });
    let mut cache = read_credits_cache(&runtime.shared_credits_path());
    let entry = cache.entries.get_mut("codex").expect("codex entry");
    assert!(entry.auth_settled);
    assert_eq!(entry.credentials_stamp, Some(7));
    entry.oauth_read_at_ms = unix_now_ms() - OAUTH_USAGE_SETTLED_TTL.as_millis() as u64 - 1;
    write_credits_cache(&runtime.shared_credits_path(), &cache);

    merge_provider_credits_entry_if_due(&runtime, "codex", Some(7), None, || {
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: unix_now_ms(),
            oauth_read_at_ms: unix_now_ms(),
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: Some(ExtraCredits::known(None, Some(8.0), None)),
            reset_credits: None,
        }
    });
    let entry = read_credits_cache(&runtime.shared_credits_path())
        .entries
        .remove("codex")
        .expect("codex entry");
    assert!(!entry.auth_settled);
    assert_eq!(entry.credentials_stamp, None);
    assert_eq!(
        entry.extra_credits,
        Some(ExtraCredits::known(None, Some(8.0), None))
    );
}

#[test]
fn fold_applies_cached_credits_and_api_spend_ceiling() {
    let mut snapshot = SidebarSnapshot::build(
        crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        Vec::new(),
        jiff::Timestamp::from_second(1_700_000_000).unwrap(),
    );
    snapshot.providers = vec![panel("claude", true), panel("codex", false)];
    let mut cache = CreditsCache::default();
    cache.entries.insert(
        "claude".to_owned(),
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 100,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: Some(ExtraCredits::known(Some(7.0), None, None)),
            reset_credits: None,
        },
    );
    let mut accounts = AccountsConfig::default();
    accounts.usage_limit_usd.insert(
        "claude".to_owned(),
        crate::config::UsageLimitUsd::from_usd(50.0),
    );
    accounts.usage_limit_usd.insert(
        "codex".to_owned(),
        crate::config::UsageLimitUsd::from_usd(25.0),
    );

    apply_credits_cache_with(&mut snapshot, &cache, &accounts, 100);
    assert_eq!(
        snapshot.providers[0].extra_credits,
        Some(ExtraCredits::known(Some(7.0), None, Some(50.0)))
    );
    assert_eq!(
        snapshot.providers[1].extra_credits,
        Some(ExtraCredits::known(Some(12.5), None, Some(25.0)))
    );
}

#[test]
fn cache_merge_preserves_other_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    merge_provider_credits(
        &runtime,
        "claude",
        Some(ExtraCredits::known(None, None, None)),
    );
    merge_provider_credits(
        &runtime,
        "codex",
        Some(ExtraCredits::known(None, Some(5.0), None)),
    );
    let cache = read_credits_cache(&runtime.shared_credits_path());
    assert!(cache.entries.contains_key("claude"));
    assert!(cache.entries.contains_key("codex"));
}

#[test]
fn extra_credits_only_merge_preserves_prior_oauth_and_reset_state() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let reset_credits = ResetCredits {
        count: 2,
        soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
    };

    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 1,
            oauth_read_at_ms: 1234,
            auth_settled: true,
            credentials_stamp: Some(11),
            account_key: Some("acc".to_owned()),
            plan: Some("pro".to_owned()),
            ok: true,
            extra_credits: None,
            reset_credits: Some(reset_credits.clone()),
        },
    );
    merge_provider_credits(
        &runtime,
        "codex",
        Some(ExtraCredits::known(None, Some(5.0), None)),
    );

    let cache = read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(
        cache
            .entries
            .get("codex")
            .and_then(|entry| entry.reset_credits.clone()),
        Some(reset_credits)
    );
    assert_eq!(
        cache
            .entries
            .get("codex")
            .map(|entry| entry.oauth_read_at_ms),
        Some(1234)
    );
    assert_eq!(
        cache.entries.get("codex").map(|entry| entry.auth_settled),
        Some(true)
    );
    assert_eq!(
        cache
            .entries
            .get("codex")
            .and_then(|entry| entry.credentials_stamp),
        Some(11)
    );
    assert_eq!(
        cache
            .entries
            .get("codex")
            .and_then(|entry| entry.account_key.as_deref()),
        Some("acc")
    );
    assert_eq!(
        cache
            .entries
            .get("codex")
            .and_then(|entry| entry.plan.as_deref()),
        Some("pro")
    );
}

#[test]
fn realtime_reset_credit_merge_preserves_paid_credit_and_oauth_state() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let paid = ExtraCredits::known(None, Some(5.0), None);
    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 1,
            oauth_read_at_ms: 1234,
            auth_settled: false,
            credentials_stamp: Some(11),
            account_key: Some("acc".to_owned()),
            plan: Some("pro".to_owned()),
            ok: true,
            extra_credits: Some(paid.clone()),
            reset_credits: None,
        },
    );
    let reset = ResetCredits {
        count: 2,
        soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
    };
    merge_provider_realtime_usage(&runtime, "codex", None, None, Some(reset.clone()));

    let cache = read_credits_cache(&runtime.shared_credits_path());
    let entry = cache.entries.get("codex").expect("codex credits");
    assert_eq!(entry.extra_credits, Some(paid));
    assert_eq!(entry.reset_credits, Some(reset));
    assert_eq!(entry.oauth_read_at_ms, 1234);
    assert_eq!(entry.account_key.as_deref(), Some("acc"));
    assert_eq!(entry.plan.as_deref(), Some("pro"));
}

#[test]
fn realtime_plan_is_displayable_replaces_prior_and_survives_missing_reads() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();

    merge_provider_realtime_usage(&runtime, "codex", Some(" team ".to_owned()), None, None);
    let cache = read_credits_cache(&runtime.shared_credits_path());
    let entry = cache.entries.get("codex").expect("plan-only entry");
    assert!(entry.ok);
    assert_eq!(entry.plan.as_deref(), Some("team"));

    merge_provider_realtime_usage(&runtime, "codex", Some("pro".to_owned()), None, None);
    merge_provider_realtime_usage(
        &runtime,
        "codex",
        None,
        Some(ExtraCredits::known(None, Some(2.0), None)),
        None,
    );
    let cache = read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(
        cache
            .entries
            .get("codex")
            .and_then(|entry| entry.plan.as_deref()),
        Some("pro")
    );
}

#[test]
fn failed_oauth_merge_preserves_prior_displayable_credits() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    let reset_credits = ResetCredits {
        count: 4,
        soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
    };
    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 42,
            oauth_read_at_ms: 1,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
            reset_credits: Some(reset_credits.clone()),
        },
    );

    merge_provider_credits_entry_if_due(&runtime, "codex", None, None, || ProviderCreditsEntry {
        scope: Default::default(),
        observed_at_ms: unix_now_ms(),
        oauth_read_at_ms: unix_now_ms(),
        auth_settled: false,
        credentials_stamp: None,
        account_key: None,
        plan: None,
        ok: false,
        extra_credits: None,
        reset_credits: None,
    });

    let entry = read_credits_cache(&runtime.shared_credits_path())
        .entries
        .remove("codex")
        .expect("codex entry");
    assert!(entry.ok);
    assert_eq!(entry.observed_at_ms, 42);
    assert_eq!(
        entry.extra_credits,
        Some(ExtraCredits::known(None, Some(5.0), None))
    );
    assert_eq!(entry.reset_credits, Some(reset_credits));
    assert!(entry.oauth_read_at_ms > 1);
}

#[test]
fn invalidate_oauth_read_zeroes_attempt_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();
    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 1,
            oauth_read_at_ms: 1234,
            auth_settled: true,
            credentials_stamp: Some(9),
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
            reset_credits: None,
        },
    );

    invalidate_oauth_read(&runtime, "codex");

    assert_eq!(
        read_credits_cache(&runtime.shared_credits_path())
            .entries
            .get("codex")
            .map(|entry| entry.oauth_read_at_ms),
        Some(0)
    );

    let mut calls = 0;
    assert!(
        merge_provider_credits_entry_if_due(&runtime, "codex", Some(9), None, || {
            calls += 1;
            ProviderCreditsEntry {
                scope: Default::default(),
                observed_at_ms: unix_now_ms(),
                oauth_read_at_ms: unix_now_ms(),
                auth_settled: false,
                credentials_stamp: None,
                account_key: None,
                plan: None,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(6.0), None)),
                reset_credits: None,
            }
        })
        .is_some(),
        "zeroed attempt stamp forces a retry even for settled auth failures"
    );
    assert_eq!(calls, 1);
}

#[test]
fn genuine_zero_reset_credits_replaces_prior_count() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.ensure_dirs().unwrap();

    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 1,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: None,
            reset_credits: Some(ResetCredits {
                count: 2,
                soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
            }),
        },
    );
    merge_provider_credits_entry(
        &runtime,
        "codex",
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 2,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: None,
            reset_credits: Some(ResetCredits {
                count: 0,
                soonest_expiry: None,
            }),
        },
    );

    let cache = read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(
        cache
            .entries
            .get("codex")
            .and_then(|entry| entry.reset_credits.as_ref())
            .map(|credits| credits.count),
        Some(0)
    );
}

#[test]
fn fold_applies_displayable_reset_credits_to_metered_panel() {
    let mut snapshot = SidebarSnapshot::build(
        crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        Vec::new(),
        jiff::Timestamp::from_second(1_700_000_000).unwrap(),
    );
    snapshot.providers = vec![panel("codex", true), panel("claude", true)];
    let reset_credits = ResetCredits {
        count: 1,
        soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
    };
    let mut cache = CreditsCache::default();
    let now_ms = CREDITS_DISPLAY_MAX_AGE.as_millis() as u64 + 100;
    cache.entries.insert(
        "codex".to_owned(),
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: now_ms,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: None,
            reset_credits: Some(reset_credits.clone()),
        },
    );
    cache.entries.insert(
        "claude".to_owned(),
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 0,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: None,
            reset_credits: Some(ResetCredits {
                count: 3,
                soonest_expiry: None,
            }),
        },
    );

    apply_credits_cache_with(&mut snapshot, &cache, &AccountsConfig::default(), now_ms);

    assert_eq!(snapshot.providers[0].reset_credits, Some(reset_credits));
    assert_eq!(snapshot.providers[1].reset_credits, None);
}

#[test]
fn fold_fills_missing_plan_from_displayable_credits_entry() {
    let mut snapshot = SidebarSnapshot::build(
        crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        Vec::new(),
        jiff::Timestamp::from_second(1_700_000_000).unwrap(),
    );
    snapshot.providers = vec![
        panel("codex", true),
        panel("claude", true),
        panel("pi", true),
    ];
    snapshot.providers[1].plan = Some("Claude Max".to_owned());
    let mut cache = CreditsCache::default();
    let now_ms = CREDITS_DISPLAY_MAX_AGE.as_millis() as u64 + 100;
    cache.entries.insert(
        "codex".to_owned(),
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: now_ms,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: Some("acc".to_owned()),
            plan: Some("pro".to_owned()),
            ok: true,
            extra_credits: None,
            reset_credits: None,
        },
    );
    cache.entries.insert(
        "claude".to_owned(),
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: now_ms,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: Some("acc".to_owned()),
            plan: Some("pro".to_owned()),
            ok: true,
            extra_credits: None,
            reset_credits: None,
        },
    );
    cache.entries.insert(
        "pi".to_owned(),
        ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 0,
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            account_key: Some("acc".to_owned()),
            plan: Some("pro".to_owned()),
            ok: true,
            extra_credits: None,
            reset_credits: None,
        },
    );

    apply_credits_cache_with(&mut snapshot, &cache, &AccountsConfig::default(), now_ms);

    assert_eq!(snapshot.providers[0].plan.as_deref(), Some("ChatGPT Pro"));
    assert_eq!(snapshot.providers[1].plan.as_deref(), Some("Claude Max"));
    assert_eq!(snapshot.providers[2].plan, None);
}
