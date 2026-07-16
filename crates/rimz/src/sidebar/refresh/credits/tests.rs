use uuid::Uuid;

use super::*;
use crate::agents::SpendTally;
use crate::ids::WorkspaceId;
use crate::{SidebarProviderPanel, SpendWindow};

fn runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn identity(
    stamp: Option<u64>,
    key: Option<&str>,
    scope: ProviderAccountScope,
) -> AccountUsageIdentity {
    AccountUsageIdentity {
        credentials_stamp: stamp,
        account_key: key.map(ToOwned::to_owned),
        scope,
    }
}

fn nonce(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn claimed_entry(nonce: Uuid, scope: ProviderAccountScope) -> ProviderCreditsEntry {
    ProviderCreditsEntry {
        scope: scope.clone(),
        observed_at_ms: 10,
        oauth_read_at_ms: 1,
        credentials_stamp: Some(7),
        account_key: Some("owner".to_owned()),
        plan: Some("pro".to_owned()),
        ok: true,
        extra_credits: Some(ExtraCredits::known(None, Some(12.0), None)),
        reset_credits: Some(ResetCredits {
            count: 3,
            soonest_expiry: None,
        }),
        direct_query_claim: Some(DirectQueryClaim {
            nonce,
            claimed_at_ms: 100,
            requested_scope: scope,
            credentials_stamp: Some(7),
            preflight_account_key: Some("owner".to_owned()),
        }),
        ..Default::default()
    }
}

#[test]
fn no_credentials_completion_preserves_same_account_display_and_settles_auth() {
    let claim_nonce = nonce(41);
    let scope = ProviderAccountScope::sub_provider("openai", "oauth");
    let prior = claimed_entry(claim_nonce, scope.clone());
    let (next, completion) = prior
        .clone()
        .complete_account_usage(
            claim_nonce,
            AccountUsageProbe::NoCredentials(identity(Some(7), Some("owner"), scope)),
            500,
        )
        .expect("matching claim completes");

    assert_eq!(next.direct_query_claim, None);
    assert_eq!(next.oauth_read_at_ms, 500);
    assert!(next.auth_settled);
    assert_eq!(next.observed_at_ms, prior.observed_at_ms);
    assert_eq!(next.ok, prior.ok);
    assert_eq!(next.plan, prior.plan);
    assert_eq!(next.extra_credits, prior.extra_credits);
    assert_eq!(next.reset_credits, prior.reset_credits);
    assert_eq!(completion.snapshot, None);
    assert!(!completion.account_changed);
}

#[test]
fn unsupported_completion_uses_claim_identity_and_preserves_same_account_display() {
    let claim_nonce = nonce(42);
    let scope = ProviderAccountScope::sub_provider("openai", "oauth");
    let prior = claimed_entry(claim_nonce, scope.clone());
    let (next, completion) = prior
        .clone()
        .complete_account_usage(claim_nonce, AccountUsageProbe::Unsupported, 600)
        .expect("matching claim completes");

    assert_eq!(next.direct_query_claim, None);
    assert_eq!(next.oauth_read_at_ms, 600);
    assert!(!next.auth_settled);
    assert_eq!(next.observed_at_ms, prior.observed_at_ms);
    assert_eq!(next.plan, prior.plan);
    assert_eq!(next.extra_credits, prior.extra_credits);
    assert_eq!(next.reset_credits, prior.reset_credits);
    assert_eq!(completion.identity.scope, scope);
    assert_eq!(completion.identity.account_key.as_deref(), Some("owner"));
    assert_eq!(completion.identity.credentials_stamp, Some(7));
    assert_eq!(completion.snapshot, None);
    assert!(!completion.account_changed);
}

fn panel(kind: &str, metered: bool) -> SidebarProviderPanel {
    SidebarProviderPanel {
        kind: kind.to_owned(),
        account_scope: Default::default(),
        product_name: kind.to_owned(),
        art: Vec::new(),
        art_tints: Vec::new(),
        color: 1,
        color_rgb: None,
        color_role: None,
        version: None,
        plan: None,
        metered,
        remote_control: Default::default(),
        active_sessions: 0,
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
fn legacy_cache_defaults_missing_claim() {
    let cache: CreditsCache = serde_json::from_str(
        r#"{"refreshed_at_ms":1,"entries":{"codex":{"observed_at_ms":1,"ok":true}}}"#,
    )
    .unwrap();
    assert_eq!(cache.entries["codex"].direct_query_claim, None);
}

#[test]
fn fold_applies_cached_credits_and_api_spend_ceiling() {
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        Vec::new(),
        jiff::Timestamp::from_second(1_700_000_000).unwrap(),
    );
    snapshot.providers = vec![panel("claude", true), panel("codex", false)];
    let cache = CreditsCache {
        entries: BTreeMap::from([(
            "claude".to_owned(),
            ProviderCreditsEntry {
                observed_at_ms: 100,
                ok: true,
                extra_credits: Some(ExtraCredits::known(Some(7.0), None, None)),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
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
fn account_usage_fresh_attempt_blocks_claim_but_identity_changes_are_due() {
    let (_dir, runtime) = runtime();
    let now = 1_000_000;
    write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    scope: ProviderAccountScope::KindWide,
                    oauth_read_at_ms: now,
                    credentials_stamp: Some(7),
                    account_key: Some("owner".to_owned()),
                    ok: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    assert_eq!(
        claim_provider_account_usage_at(
            &runtime,
            "codex",
            identity(Some(7), Some("owner"), ProviderAccountScope::KindWide),
            now + 1,
            nonce(1),
        ),
        None
    );
    for (index, (stamp, key, scope)) in [
        (Some(8), Some("owner"), ProviderAccountScope::KindWide),
        (Some(7), Some("other"), ProviderAccountScope::KindWide),
        (None, None, ProviderAccountScope::KindWide),
        (
            Some(7),
            Some("owner"),
            ProviderAccountScope::sub_provider("openai", "oauth"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        write_credits_cache(
            &runtime.shared_credits_path(),
            &CreditsCache {
                entries: BTreeMap::from([(
                    "codex".to_owned(),
                    ProviderCreditsEntry {
                        scope: ProviderAccountScope::KindWide,
                        oauth_read_at_ms: now,
                        credentials_stamp: Some(7),
                        account_key: Some("owner".to_owned()),
                        ok: true,
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        let claim_id = nonce(index as u128 + 2);
        assert!(
            claim_provider_account_usage_at(
                &runtime,
                "codex",
                identity(stamp, key, scope),
                now + 2,
                claim_id,
            )
            .is_some()
        );
        cancel_provider_account_usage_claim(&runtime, "codex", claim_id);
    }

    write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    oauth_read_at_ms: now,
                    credentials_stamp: Some(7),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    assert!(
        claim_provider_account_usage_at(
            &runtime,
            "codex",
            identity(Some(7), Some("owner"), ProviderAccountScope::KindWide),
            now + 2,
            nonce(9),
        )
        .is_some()
    );
}

#[test]
fn settled_attempt_waits_for_long_ttl_unless_credentials_change() {
    let (_dir, runtime) = runtime();
    let now = 1_000_000;
    write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "claude".to_owned(),
                ProviderCreditsEntry {
                    oauth_read_at_ms: now,
                    auth_settled: true,
                    credentials_stamp: Some(7),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    assert_eq!(
        claim_provider_account_usage_at(
            &runtime,
            "claude",
            identity(Some(7), None, ProviderAccountScope::KindWide),
            now + OAUTH_USAGE_TTL.as_millis() as u64 + 1,
            nonce(1),
        ),
        None
    );
    assert_eq!(
        claim_provider_account_usage_at(
            &runtime,
            "claude",
            identity(Some(8), None, ProviderAccountScope::KindWide),
            now + 1,
            nonce(2),
        ),
        Some(nonce(2))
    );
}

#[test]
fn completion_detects_symmetric_owner_and_scope_changes() {
    let sub = ProviderAccountScope::sub_provider("openai", "oauth");
    for (prior_key, current_key, prior_scope, current_scope, expected) in [
        (
            None,
            Some("owner"),
            ProviderAccountScope::KindWide,
            ProviderAccountScope::KindWide,
            true,
        ),
        (
            Some("owner"),
            None,
            ProviderAccountScope::KindWide,
            ProviderAccountScope::KindWide,
            true,
        ),
        (
            Some("old"),
            Some("new"),
            ProviderAccountScope::KindWide,
            ProviderAccountScope::KindWide,
            true,
        ),
        (
            Some("owner"),
            Some("owner"),
            ProviderAccountScope::KindWide,
            sub.clone(),
            true,
        ),
        (Some("owner"), Some("owner"), sub.clone(), sub, false),
    ] {
        let (_dir, runtime) = runtime();
        write_credits_cache(
            &runtime.shared_credits_path(),
            &CreditsCache {
                entries: BTreeMap::from([(
                    "codex".to_owned(),
                    ProviderCreditsEntry {
                        scope: prior_scope,
                        account_key: prior_key.map(ToOwned::to_owned),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        let current = identity(None, current_key, current_scope);
        let claim =
            claim_provider_account_usage_at(&runtime, "codex", current.clone(), 100, nonce(1))
                .unwrap();
        let completion = complete_provider_account_usage(
            &runtime,
            "codex",
            claim,
            AccountUsageProbe::Found {
                identity: current,
                snapshot: AccountUsageSnapshot::default(),
            },
        )
        .unwrap();
        assert_eq!(completion.account_changed, expected);
    }
}

#[test]
fn competing_claim_waits_and_expired_claim_is_replaced() {
    let (_dir, runtime) = runtime();
    let first = nonce(1);
    let second = nonce(2);
    let hint = identity(None, None, ProviderAccountScope::KindWide);
    assert_eq!(
        claim_provider_account_usage_at(&runtime, "codex", hint.clone(), 100, first),
        Some(first)
    );
    assert_eq!(
        claim_provider_account_usage_at(&runtime, "codex", hint.clone(), 101, second),
        None
    );
    assert_eq!(
        claim_provider_account_usage_at(
            &runtime,
            "codex",
            hint.clone(),
            100 + ACCOUNT_USAGE_CLAIM_TTL.as_millis() as u64,
            second,
        ),
        None,
        "the lease remains live at its exact boundary"
    );
    let expired = 100 + ACCOUNT_USAGE_CLAIM_TTL.as_millis() as u64 + 1;
    assert_eq!(
        claim_provider_account_usage_at(&runtime, "codex", hint, expired, second),
        Some(second)
    );
    assert!(!account_usage_claim_matches(&runtime, "codex", first));
    assert!(account_usage_claim_matches(&runtime, "codex", second));
}

#[test]
fn renewal_requires_the_matching_live_nonce() {
    let (_dir, runtime) = runtime();
    let first = nonce(1);
    claim_provider_account_usage_at(&runtime, "codex", Default::default(), 100, first).unwrap();

    assert!(renew_provider_account_usage_claim_at(
        &runtime, "codex", first, 200
    ));
    assert_eq!(
        read_credits_cache(&runtime.shared_credits_path()).entries["codex"]
            .direct_query_claim
            .as_ref()
            .map(|claim| claim.claimed_at_ms),
        Some(200)
    );
    assert!(!renew_provider_account_usage_claim_at(
        &runtime,
        "codex",
        nonce(2),
        300
    ));

    let second = nonce(2);
    claim_provider_account_usage_at(
        &runtime,
        "codex",
        Default::default(),
        200 + ACCOUNT_USAGE_CLAIM_TTL.as_millis() as u64 + 1,
        second,
    )
    .unwrap();
    assert!(!renew_provider_account_usage_claim_at(
        &runtime, "codex", first, 400
    ));
    let claim = read_credits_cache(&runtime.shared_credits_path()).entries["codex"]
        .direct_query_claim
        .clone()
        .unwrap();
    assert_eq!(claim.nonce, second);
    assert_ne!(claim.claimed_at_ms, 400);
}

#[test]
fn cancel_and_late_completion_require_matching_nonce() {
    let (_dir, runtime) = runtime();
    let first =
        claim_provider_account_usage_at(&runtime, "codex", Default::default(), 100, nonce(1))
            .unwrap();
    assert!(cancel_provider_account_usage_claim(
        &runtime, "codex", first
    ));
    assert!(!cancel_provider_account_usage_claim(
        &runtime, "codex", first
    ));

    let second =
        claim_provider_account_usage_at(&runtime, "codex", Default::default(), 101, nonce(2))
            .unwrap();
    assert!(
        complete_provider_account_usage(
            &runtime,
            "codex",
            first,
            AccountUsageProbe::Failed(Default::default()),
        )
        .is_none()
    );
    assert!(account_usage_claim_matches(&runtime, "codex", second));
}

#[test]
fn failed_read_preserves_display_data_and_advances_attempt() {
    let (_dir, runtime) = runtime();
    let prior = ProviderCreditsEntry {
        scope: ProviderAccountScope::sub_provider("openai", "oauth"),
        observed_at_ms: 10,
        oauth_read_at_ms: 1,
        credentials_stamp: Some(7),
        account_key: Some("owner".to_owned()),
        plan: Some("pro".to_owned()),
        ok: true,
        extra_credits: Some(ExtraCredits::known(None, Some(12.0), None)),
        ..Default::default()
    };
    write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([("codex".to_owned(), prior.clone())]),
            ..Default::default()
        },
    );
    invalidate_oauth_read(&runtime, "codex");
    let claim = claim_provider_account_usage_at(
        &runtime,
        "codex",
        identity(None, Some("owner"), ProviderAccountScope::KindWide),
        100,
        nonce(1),
    )
    .unwrap();
    complete_provider_account_usage(
        &runtime,
        "codex",
        claim,
        AccountUsageProbe::Failed(Default::default()),
    )
    .unwrap();
    let entry = read_credits_cache(&runtime.shared_credits_path())
        .entries
        .remove("codex")
        .unwrap();
    assert!(entry.ok);
    assert_eq!(entry.plan, prior.plan);
    assert_eq!(entry.extra_credits, prior.extra_credits);
    assert_eq!(entry.scope, prior.scope);
    assert_eq!(entry.credentials_stamp, prior.credentials_stamp);
    assert_eq!(entry.account_key, prior.account_key);
    assert!(entry.oauth_read_at_ms > 1);
    assert_eq!(entry.direct_query_claim, None);
}

#[test]
fn failed_read_uses_claimed_owner_when_probe_cannot_repeat_identity() {
    let (_dir, runtime) = runtime();
    write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "plugin".to_owned(),
                ProviderCreditsEntry {
                    account_key: Some("old".to_owned()),
                    plan: Some("pro".to_owned()),
                    ok: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    let claim = claim_provider_account_usage_at(
        &runtime,
        "plugin",
        identity(None, Some("new"), ProviderAccountScope::KindWide),
        100,
        nonce(1),
    )
    .unwrap();
    let completion = complete_provider_account_usage(
        &runtime,
        "plugin",
        claim,
        AccountUsageProbe::Failed(Default::default()),
    )
    .unwrap();
    assert!(completion.account_changed);
    let entry = &read_credits_cache(&runtime.shared_credits_path()).entries["plugin"];
    assert_eq!(entry.account_key.as_deref(), Some("new"));
    assert_eq!(entry.plan, None);
}

#[test]
fn successful_partial_read_preserves_prior_optional_credits() {
    let (_dir, runtime) = runtime();
    let current = identity(None, Some("owner"), ProviderAccountScope::KindWide);
    write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    account_key: Some("owner".to_owned()),
                    plan: Some("pro".to_owned()),
                    extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
                    reset_credits: Some(ResetCredits {
                        count: 3,
                        soonest_expiry: None,
                    }),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    let claim =
        claim_provider_account_usage_at(&runtime, "codex", current.clone(), 100, nonce(1)).unwrap();
    complete_provider_account_usage(
        &runtime,
        "codex",
        claim,
        AccountUsageProbe::Found {
            identity: current,
            snapshot: AccountUsageSnapshot::default(),
        },
    )
    .unwrap();
    let entry = &read_credits_cache(&runtime.shared_credits_path()).entries["codex"];
    assert_eq!(entry.plan.as_deref(), Some("pro"));
    assert_eq!(
        entry.extra_credits,
        Some(ExtraCredits::known(None, Some(5.0), None))
    );
    assert_eq!(
        entry.reset_credits.as_ref().map(|credits| credits.count),
        Some(3)
    );
}

#[test]
fn realtime_write_preserves_attempt_and_claim() {
    let (_dir, runtime) = runtime();
    let claim = DirectQueryClaim {
        nonce: nonce(1),
        claimed_at_ms: 100,
        requested_scope: ProviderAccountScope::KindWide,
        credentials_stamp: Some(7),
        preflight_account_key: Some("owner".to_owned()),
    };
    write_credits_cache(
        &runtime.shared_credits_path(),
        &CreditsCache {
            entries: BTreeMap::from([(
                "codex".to_owned(),
                ProviderCreditsEntry {
                    oauth_read_at_ms: 55,
                    auth_settled: true,
                    credentials_stamp: Some(7),
                    account_key: Some("owner".to_owned()),
                    extra_credits: Some(ExtraCredits::known(None, Some(9.0), None)),
                    reset_credits: Some(ResetCredits {
                        count: 2,
                        soonest_expiry: None,
                    }),
                    direct_query_claim: Some(claim.clone()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    merge_provider_realtime_usage(
        &runtime,
        "codex",
        ProviderAccountScope::KindWide,
        AccountUsageSnapshot {
            plan: Some(" pro ".to_owned()),
            ..Default::default()
        },
    );
    let entry = &read_credits_cache(&runtime.shared_credits_path()).entries["codex"];
    assert_eq!(entry.oauth_read_at_ms, 55);
    assert!(entry.auth_settled);
    assert_eq!(entry.direct_query_claim.as_ref(), Some(&claim));
    assert_eq!(entry.plan.as_deref(), Some("pro"));
    assert_eq!(
        entry.extra_credits,
        Some(ExtraCredits::known(None, Some(9.0), None))
    );
    assert_eq!(
        entry.reset_credits.as_ref().map(|credits| credits.count),
        Some(2)
    );

    merge_provider_realtime_usage(
        &runtime,
        "codex",
        ProviderAccountScope::KindWide,
        AccountUsageSnapshot {
            reset_credits: Some(ResetCredits {
                count: 0,
                soonest_expiry: None,
            }),
            ..Default::default()
        },
    );
    let entry = &read_credits_cache(&runtime.shared_credits_path()).entries["codex"];
    assert_eq!(entry.oauth_read_at_ms, 55);
    assert_eq!(entry.direct_query_claim.as_ref(), Some(&claim));
    assert_eq!(
        entry.extra_credits,
        Some(ExtraCredits::known(None, Some(9.0), None))
    );
    assert_eq!(
        entry.reset_credits.as_ref().map(|credits| credits.count),
        Some(0)
    );
}
