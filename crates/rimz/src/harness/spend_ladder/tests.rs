use super::*;
use crate::agents::{RateLimitWindow, RedeemEffect};
use crate::config::SpendOrder;

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
        }),
        rate_pct_per_day: None,
        last_attempt: None,
        supports_reset: true,
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
        let config = ResumeConfig {
            auto_redeem: true,
            spend_order,
            auto_redeem_min_gain: min_gain.into(),
            ..Default::default()
        };
        assert_eq!(facts.step(&config, None, now()), expected);
    }
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
                let config = ResumeConfig {
                    auto_redeem: true,
                    spend_order,
                    auto_redeem_min_gain: min_gain.into(),
                    ..Default::default()
                };
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
            let config = ResumeConfig {
                auto_redeem: true,
                spend_order,
                ..Default::default()
            };
            assert_eq!(facts.step(&config, None, now()), SpendStep::Subscription);
            facts.capacity = None;
            assert_eq!(facts.step(&config, None, now()), SpendStep::Subscription);
        }
    }
}

#[test]
fn a_recent_failure_or_a_stalled_reservation_closes_the_reset_step() {
    let config = ResumeConfig {
        auto_redeem: true,
        ..Default::default()
    };
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
        key.clone(),
        crate::agents::account::RateLimitCacheEntry {
            limits: crate::agents::AgentRateLimits {
                windows: capacity.windows,
            },
            ..Default::default()
        },
    );
    crate::disk::atomic::write_temp_then_rename_cache(&runtime.shared_rate_limits_path(), &rates)
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
            SpendFacts::read(&runtime, &key, now()).step(&ResumeConfig::default(), None, now()),
            expected
        );
    }
}
