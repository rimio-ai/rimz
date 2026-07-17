use super::*;
use crate::agents::{AgentRateLimits, ProviderAccountScope, RateLimitWindow};
use crate::ids::WorkspaceId;
use jiff::SignedDuration;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

fn spent_capacity(now: Timestamp, gain: Duration) -> ProviderCapacity {
    ProviderCapacity::from_windows(vec![RateLimitWindow {
        used_percentage: Some(100),
        resets_at: Some(now + gain),
        duration_mins: Some(10_080),
        ..Default::default()
    }])
}

fn undated_spent_capacity() -> ProviderCapacity {
    ProviderCapacity::from_windows(vec![RateLimitWindow {
        used_percentage: Some(100),
        resets_at: None,
        duration_mins: Some(10_080),
        ..Default::default()
    }])
}

fn credits(now: Timestamp, expiry: Option<Duration>) -> ResetCredits {
    ResetCredits {
        count: 1,
        soonest_expiry: expiry.map(|duration| now + duration),
    }
}

fn verdict(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    now: Timestamp,
) -> Option<RedeemReason> {
    redeem_verdict(capacity, credits, Duration::from_secs(12 * 3600), true, now)
}

#[test]
fn verdict_covers_gain_hold_and_missing_data_matrix() {
    let now = ts(1_700_000_000);

    let blocked_one_day = spent_capacity(now, Duration::from_secs(24 * 3600));
    assert_eq!(
        verdict(
            Some(&blocked_one_day),
            &credits(now, Some(Duration::from_secs(25 * 3600))),
            now,
        ),
        Some(RedeemReason::DoomedCredit),
        "24h gain plus 1h hold is doomed"
    );

    let blocked_ten_minutes = spent_capacity(now, Duration::from_secs(10 * 60));
    assert_eq!(
        verdict(
            Some(&blocked_ten_minutes),
            &credits(now, Some(Duration::from_secs(7 * 86_400))),
            now,
        ),
        None,
    );

    let blocked_four_hours = spent_capacity(now, Duration::from_secs(4 * 3600));
    assert_eq!(
        verdict(
            Some(&blocked_four_hours),
            &credits(now, Some(Duration::from_secs(40 * 3600))),
            now,
        ),
        None,
        "4h gain plus 36h hold waits"
    );

    let blocked_three_days = spent_capacity(now, Duration::from_secs(3 * 86_400));
    assert_eq!(
        verdict(
            Some(&blocked_three_days),
            &credits(now, Some(Duration::from_secs(10 * 86_400))),
            now,
        ),
        Some(RedeemReason::BlockedGain),
    );

    let blocked_five_hours = spent_capacity(now, Duration::from_secs(5 * 3600));
    assert_eq!(
        verdict(
            Some(&blocked_five_hours),
            &credits(now, Some(Duration::from_secs(4 * 3600))),
            now,
        ),
        Some(RedeemReason::DoomedCredit),
        "a 5h-only block redeems only when the credit dies first"
    );

    assert_eq!(
        verdict(None, &credits(now, Some(Duration::from_secs(30 * 60))), now,),
        Some(RedeemReason::ExpiryRescue),
    );
    assert_eq!(
        verdict(
            Some(&blocked_four_hours),
            &credits(now, Some(Duration::from_secs(20 * 60))),
            now,
        ),
        Some(RedeemReason::ExpiryRescue),
        "rescue wins over every limit reason"
    );
    assert_eq!(
        verdict(Some(&blocked_four_hours), &credits(now, None), now),
        None,
    );
    assert_eq!(
        verdict(Some(&undated_spent_capacity()), &credits(now, None), now),
        None,
        "a spent window without a future reset cannot redeem"
    );
    assert_eq!(
        verdict(
            None,
            &ResetCredits {
                count: 0,
                soonest_expiry: Some(now + Duration::from_secs(60)),
            },
            now,
        ),
        None,
    );
}

#[test]
fn limit_redemption_requires_opt_in_but_rescue_does_not() {
    let now = ts(1_700_000_000);
    let blocked = spent_capacity(now, Duration::from_secs(3 * 86_400));
    assert_eq!(
        redeem_verdict(
            Some(&blocked),
            &credits(now, None),
            Duration::from_secs(12 * 3600),
            false,
            now,
        ),
        None,
    );
    assert_eq!(
        redeem_verdict(
            Some(&blocked),
            &credits(now, Some(Duration::from_secs(10 * 60))),
            Duration::from_secs(12 * 3600),
            false,
            now,
        ),
        Some(RedeemReason::ExpiryRescue),
    );
}

#[test]
fn stamp_cooldowns_distinguish_attempts_and_successes() {
    let now = ts(1_700_000_000);
    let mut stamp = RedeemStamp {
        attempted_at: now,
        request_id: "request".to_owned(),
        reason: RedeemReason::BlockedGain,
        outcome: None,
    };
    assert!(!stamp_allows_attempt(
        Some(&stamp),
        now + Duration::from_secs(599)
    ));
    assert!(stamp_allows_attempt(Some(&stamp), now + ATTEMPT_COOLDOWN));

    stamp.outcome = Some("reset".to_owned());
    assert!(!stamp_allows_attempt(
        Some(&stamp),
        now + Duration::from_secs(1799)
    ));
    assert!(stamp_allows_attempt(
        Some(&stamp),
        now + POST_SUCCESS_COOLDOWN
    ));
    assert!(!stamp_allows_attempt(
        Some(&stamp),
        now - SignedDuration::from_secs(1)
    ));
}

#[test]
fn stamp_round_trips_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_auto_redeem_path("codex");
    let stamp = RedeemStamp {
        attempted_at: ts(1_700_000_000),
        request_id: "0195-request".to_owned(),
        reason: RedeemReason::DoomedCredit,
        outcome: Some("nothing_to_reset".to_owned()),
    };

    write_stamp(&path, &stamp).unwrap();

    assert_eq!(read_stamp(&path), Some(stamp));
}

#[test]
fn producer_reserves_a_spawn_and_paces_the_next_tick() {
    let now = ts(1_700_000_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    crate::sidebar::refresh::merge_account_rate_limits(
        &runtime,
        CODEX_KIND,
        ProviderAccountScope::KindWide,
        AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(100),
                resets_at: Some(now + Duration::from_secs(3 * 86_400)),
                duration_mins: Some(10_080),
                ..Default::default()
            }],
        },
    );
    let mut panel = crate::sidebar::test_support::provider_panel(CODEX_KIND, Vec::new());
    panel.reset_credits = Some(credits(now, Some(Duration::from_secs(10 * 86_400))));
    let config = ResumeConfig {
        auto_redeem: true,
        ..Default::default()
    };

    redeem_credits(std::slice::from_ref(&panel), &runtime, &config, now);
    let first = read_stamp(&runtime.shared_auto_redeem_path(CODEX_KIND)).unwrap();
    assert_eq!(first.attempted_at, now);
    assert_eq!(first.reason, RedeemReason::BlockedGain);
    assert_eq!(first.outcome, None);

    redeem_credits(
        std::slice::from_ref(&panel),
        &runtime,
        &config,
        now + Duration::from_secs(1),
    );
    assert_eq!(
        read_stamp(&runtime.shared_auto_redeem_path(CODEX_KIND)),
        Some(first),
        "the pending reservation must pace producer ticks before the helper reports an outcome"
    );
}

#[test]
fn spawn_failure_cancels_only_its_matching_reservation() {
    let now = ts(1_700_000_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_auto_redeem_path(CODEX_KIND);

    assert!(reserve_attempt(
        &runtime,
        RedeemReason::ExpiryRescue,
        now,
        "request-a"
    ));
    cancel_attempt_reservation(&runtime, "request-b");
    assert_eq!(read_stamp(&path).unwrap().request_id, "request-a");

    cancel_attempt_reservation(&runtime, "request-a");
    assert!(read_stamp(&path).is_none());
}

#[test]
fn credit_selection_prefers_the_soonest_known_expiry() {
    let details = vec![
        ResetCreditDetail {
            id: Some("undated".to_owned()),
            expires_at: None,
        },
        ResetCreditDetail {
            id: Some("later".to_owned()),
            expires_at: Some(ts(300)),
        },
        ResetCreditDetail {
            id: Some("earlier".to_owned()),
            expires_at: Some(ts(200)),
        },
    ];

    assert_eq!(soonest_credit_id(&details), Some("earlier"));
    assert_eq!(
        soonest_credit_id(&[ResetCreditDetail {
            id: None,
            expires_at: None,
        }]),
        None
    );
}
