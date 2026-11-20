use super::*;
use crate::agents::{RateLimitWindow, RedeemEffect};
use crate::config::SpendOrder;

fn config(spend_order: SpendOrder, min_gain: &str) -> AutoRedeemConfig {
    let mut config = AutoRedeemConfig::default();
    config.level = RedeemLevel::Paced;
    config.spend_order = spend_order;
    config.min_gain = min_gain.into();
    config
}

fn now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn facts(used: u8, extra_credits: Option<ExtraCredits>) -> SpendFacts {
    SpendFacts {
        capacity: Some(ProviderCapacity::from_windows(vec![RateLimitWindow {
            used_percentage: Some(used),
            duration_mins: Some(10_080),
            resets_at: Some(now() + std::time::Duration::from_secs(3 * 86_400)),
            ..Default::default()
        }])),
        extra_credits,
        reset_credits: Some(ResetCredits {
            count: 1,
            soonest_expiry: None,
            expiries: Vec::new(),
            effect: RedeemEffect::RestartsWindow,
            grants: Vec::new(),
        }),
        rate_pct_per_day: None,
        last_attempt: None,
        kind: AgentKind::new_unchecked("codex"),
    }
}

#[test]
fn paid_credit_precedes_resets_unless_resets_first_can_redeem() {
    let facts = facts(100, Some(ExtraCredits::known(None, Some(100.0), None)));
    for (spend_order, min_gain, expected) in [
        (SpendOrder::CreditsFirst, "12h", SpendStep::PaidCredit),
        (SpendOrder::ResetsFirst, "12h", SpendStep::ResetCredit),
        (SpendOrder::ResetsFirst, "4d", SpendStep::PaidCredit),
    ] {
        let config = config(spend_order, min_gain);
        assert_eq!(facts.step(&config, None, now()), expected);
    }
}

#[test]
fn the_ladder_reads_the_policy_of_its_own_kind() {
    let facts = facts(100, Some(ExtraCredits::known(None, Some(100.0), None)));
    let codex_redeems: AutoRedeemConfig = toml::from_str(
        "min_gain = \"4d\"\n[codex]\nlevel = \"gain\"\nmin_gain = \"12h\"\nspend_order = \"resets_first\"\n",
    )
    .unwrap();
    assert_eq!(
        facts.step(&codex_redeems, None, now()),
        SpendStep::ResetCredit
    );
    let codex_rescues: AutoRedeemConfig = toml::from_str(
        "level = \"paced\"\nspend_order = \"resets_first\"\n[codex]\nlevel = \"rescue\"\n",
    )
    .unwrap();
    assert_eq!(
        facts.step(&codex_rescues, None, now()),
        SpendStep::PaidCredit
    );
}

#[test]
fn extra_out_falls_through_to_resets_then_spent() {
    for extra in [
        None,
        Some(ExtraCredits::Disabled),
        Some(ExtraCredits::known(None, Some(0.0), None)),
    ] {
        for spend_order in [SpendOrder::CreditsFirst, SpendOrder::ResetsFirst] {
            let facts = facts(100, extra.clone());
            for (min_gain, expected) in [("12h", SpendStep::ResetCredit), ("4d", SpendStep::Spent)]
            {
                let config = config(spend_order, min_gain);
                assert_eq!(facts.step(&config, None, now()), expected);
            }
        }
    }
}

#[test]
fn subscription_room_and_unknown_capacity_win_over_credits() {
    for extra in [
        None,
        Some(ExtraCredits::Disabled),
        Some(ExtraCredits::known(None, None, None)),
    ] {
        let mut facts = facts(40, extra);
        for spend_order in [SpendOrder::CreditsFirst, SpendOrder::ResetsFirst] {
            let config = config(spend_order, "12h");
            assert_eq!(facts.step(&config, None, now()), SpendStep::Subscription);
            facts.capacity = None;
            assert_eq!(facts.step(&config, None, now()), SpendStep::Subscription);
        }
    }
}

#[test]
fn a_recent_failure_or_a_stalled_reservation_closes_the_reset_step() {
    let config = config(SpendOrder::CreditsFirst, "12h");
    let mut facts = facts(100, None);
    for (outcome, attempted_at, expected) in [
        (Some("no_credit"), now(), SpendStep::Spent),
        (
            Some("no_credit"),
            now() - std::time::Duration::from_secs(601),
            SpendStep::ResetCredit,
        ),
        (None, now(), SpendStep::ResetCredit),
        (
            None,
            now() - std::time::Duration::from_secs(120),
            SpendStep::Spent,
        ),
        (
            None,
            now() - std::time::Duration::from_secs(601),
            SpendStep::ResetCredit,
        ),
        (Some("reset"), now(), SpendStep::ResetCredit),
    ] {
        facts.last_attempt = Some(
            serde_json::from_value(serde_json::json!({
                "attempted_at": attempted_at,
                "request_id": "attempt",
                "reason": "blocked_gain",
                "outcome": outcome,
            }))
            .unwrap(),
        );
        assert_eq!(facts.step(&config, None, now()), expected);
    }
}

#[test]
fn cache_evidence_excludes_display_only_limits_stale_scoped_and_lapsed_entries() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let key: LoginKey = "codex@default".parse().unwrap();
    let capacity = facts(100, None).capacity.unwrap();
    let mut rates = crate::agents::account::RateLimitsCache::default();
    rates.entries.insert(
        key.clone().into(),
        crate::agents::account::RateLimitCacheEntry {
            limits: crate::agents::AgentRateLimits {
                windows: capacity.windows,
            },
            ..Default::default()
        },
    );
    crate::disk::atomic::write_temp_then_rename_cache(
        &crate::agents::account::RateLimitsCache::shared_path(&runtime),
        &rates,
    )
    .unwrap();
    let mut cache = crate::agents::credits::CreditsCache::default();
    let entry = crate::agents::credits::ProviderCreditsEntry {
        ok: true,
        observed_at_ms: now().as_millisecond() as u64,
        extra_credits: Some(ExtraCredits::known(None, None, None)),
        ..Default::default()
    };
    for (entry, expected) in [
        (None, SpendStep::Spent),
        (Some(entry.clone()), SpendStep::PaidCredit),
        (
            Some(crate::agents::credits::ProviderCreditsEntry {
                ok: false,
                ..entry.clone()
            }),
            SpendStep::Spent,
        ),
        (
            Some(crate::agents::credits::ProviderCreditsEntry {
                observed_at_ms: 1,
                ..entry.clone()
            }),
            SpendStep::Spent,
        ),
        (
            Some(crate::agents::credits::ProviderCreditsEntry {
                scope: crate::agents::ProviderAccountScope::sub_provider("codex", "team"),
                ..entry.clone()
            }),
            SpendStep::Spent,
        ),
        (
            Some(crate::agents::credits::ProviderCreditsEntry {
                entitlement: crate::agents::Entitlement::Lapsed { since_ms: 1 },
                ..entry.clone()
            }),
            SpendStep::Spent,
        ),
    ] {
        cache.logins.clear();
        if let Some(entry) = entry {
            cache.logins.insert(key.clone(), entry);
        }
        crate::disk::atomic::write_temp_then_rename_cache(&runtime.shared_credits_path(), &cache)
            .unwrap();
        assert_eq!(
            SpendFacts::read(&runtime, &key, now()).step(&AutoRedeemConfig::default(), None, now()),
            expected
        );
    }
}

#[test]
fn a_claude_login_on_a_dry_window_steps_to_a_reset_only_on_a_grant_that_clears_it() {
    let mut facts = facts(100, None);
    facts.kind = AgentKind::new_unchecked("claude");
    let grant = |clears: &[&str]| ResetCredits {
        count: 1,
        soonest_expiry: None,
        expiries: Vec::new(),
        effect: RedeemEffect::KeepsSchedule,
        grants: vec![crate::agents::ResetGrant {
            count: 1,
            clears: clears.iter().map(|clear| (*clear).to_owned()).collect(),
            selected: true,
            usable_now: true,
            ..Default::default()
        }],
    };
    let config = config(SpendOrder::CreditsFirst, "12h");
    let lead = std::time::Duration::from_secs(86_400);
    facts.reset_credits = Some(grant(&["five_hour", "seven_day"]));
    assert_eq!(facts.step(&config, None, now()), SpendStep::ResetCredit);
    assert_eq!(
        facts.redemption(&config, now()),
        Some(RedeemReason::BlockedGain)
    );
    let rescue = AutoRedeemConfig::default();
    assert_eq!(facts.step(&rescue, None, now()), SpendStep::Spent);
    for config in [&config, &rescue] {
        assert_eq!(
            facts.advice(config, lead, now()),
            Some(RedeemReason::BlockedGain),
            "advice as if auto-redeem ran at its fullest (level: {:?})",
            config.level
        );
    }

    facts.reset_credits = Some(grant(&["five_hour", "seven_day_overage_included"]));
    assert_eq!(facts.step(&config, None, now()), SpendStep::Spent);
    assert_eq!(facts.redemption(&config, now()), None);
    assert_eq!(facts.advice(&config, lead, now()), None);
}

#[test]
fn a_claude_rescue_of_a_grant_that_leaves_the_week_spent_keeps_the_ladder_spent() {
    let mut facts = facts(100, None);
    facts.kind = AgentKind::new_unchecked("claude");
    if let Some(capacity) = facts.capacity.as_mut() {
        capacity.windows.push(RateLimitWindow {
            used_percentage: Some(40),
            duration_mins: Some(300),
            resets_at: Some(now() + std::time::Duration::from_secs(2 * 3_600)),
            ..Default::default()
        });
    }
    let expiry = now() + std::time::Duration::from_secs(20 * 60);
    facts.reset_credits = Some(ResetCredits {
        count: 1,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry],
        effect: RedeemEffect::KeepsSchedule,
        grants: vec![crate::agents::ResetGrant {
            count: 1,
            expiry: Some(expiry),
            clears: vec!["five_hour".to_owned()],
            selected: true,
            usable_now: true,
            ..Default::default()
        }],
    });
    for config in [
        &config(SpendOrder::CreditsFirst, "12h"),
        &AutoRedeemConfig::default(),
    ] {
        assert_eq!(
            facts.step(config, None, now()),
            SpendStep::Spent,
            "a five-hour refill leaves the week spent (level: {:?})",
            config.level
        );
        assert_eq!(
            facts.redemption(config, now()),
            Some(RedeemReason::ExpiryRescue),
            "the helper still rescues the dying grant (level: {:?})",
            config.level
        );
    }
}
