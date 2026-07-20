use super::*;
use crate::agents::{AgentRateLimits, RateLimitWindow};
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
        expiries: Vec::new(),
    }
}

fn verdict(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    now: Timestamp,
) -> Option<RedeemReason> {
    redeem_verdict(
        capacity,
        credits,
        None,
        Duration::from_secs(12 * 3600),
        true,
        now,
    )
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
                expiries: Vec::new(),
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
            None,
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
            None,
            Duration::from_secs(12 * 3600),
            false,
            now,
        ),
        Some(RedeemReason::ExpiryRescue),
    );
}

#[test]
fn rate_stamp_learns_growth_and_restarts_at_window_edges() {
    let observed = ts(1_700_000_000);
    let reset = observed + Duration::from_secs(7 * 86_400);
    let window = |used_percentage, resets_at, observed_at| RateLimitWindow {
        used_percentage: Some(used_percentage),
        resets_at: Some(resets_at),
        duration_mins: Some(10_080),
        observed_at: Some(observed_at),
        ..Default::default()
    };

    let first = update_rate_stamp(None, &window(10, reset, observed)).unwrap();
    assert_eq!(first.rate_pct_per_day, 0.0);

    let learned = update_rate_stamp(
        Some(&first),
        &window(30, reset, observed + Duration::from_secs(86_400)),
    )
    .unwrap();
    assert_eq!(learned.rate_pct_per_day, 20.0);

    let folded = update_rate_stamp(
        Some(&learned),
        &window(40, reset, observed + Duration::from_secs(2 * 86_400)),
    )
    .unwrap();
    let alpha = 1.0 - 0.5_f64.powf(1.0 / 3.0);
    let expected = 20.0 + alpha * (10.0 - 20.0);
    assert!((folded.rate_pct_per_day - expected).abs() < 1e-9);

    let next_reset = reset + Duration::from_secs(86_400);
    let restarted = update_rate_stamp(
        Some(&folded),
        &window(1, next_reset, observed + Duration::from_secs(3 * 86_400)),
    )
    .unwrap();
    assert_eq!(restarted.window_resets_at, next_reset);
    assert_eq!(restarted.last_used_pct, 1);
    assert_eq!(restarted.rate_pct_per_day, folded.rate_pct_per_day);

    let stale = update_rate_stamp(
        Some(&restarted),
        &window(90, next_reset, observed + Duration::from_secs(2 * 86_400)),
    )
    .unwrap();
    assert_eq!(stale, restarted, "out-of-order observations are ignored");
}

#[test]
fn chain_deadlines_space_refills_and_fall_back_to_rescue() {
    let now = ts(1_700_000_000);
    let expiry = now + Duration::from_secs(20 * 86_400);
    let expiries = [expiry, expiry, expiry];
    let refill = Duration::from_secs(5 * 86_400);
    let rescue = expiry - EXPIRY_RESCUE_LEAD;

    assert_eq!(chain_deadline(&expiries[..1], Some(20.0)), Some(rescue));
    assert_eq!(
        chain_deadline(&expiries, Some(20.0)),
        Some(rescue - refill - refill)
    );
    assert_eq!(
        chain_deadline(&expiries, Some(RATE_FLOOR - 0.01)),
        Some(rescue)
    );

    let credits = ResetCredits {
        count: 3,
        soonest_expiry: Some(expiry),
        expiries: expiries.to_vec(),
    };
    let deadline = rescue - refill - refill;
    assert_eq!(
        redeem_verdict(
            None,
            &credits,
            Some(20.0),
            Duration::from_secs(12 * 3_600),
            true,
            deadline - Duration::from_secs(1),
        ),
        None,
    );
    assert_eq!(
        redeem_verdict(
            None,
            &credits,
            Some(20.0),
            Duration::from_secs(12 * 3_600),
            true,
            deadline,
        ),
        Some(RedeemReason::ScheduledRedeem),
    );
}

#[test]
fn near_free_reset_defers_only_a_credit_that_comfortably_survives() {
    let now = ts(1_700_000_000);
    let reset = now + Duration::from_secs(60 * 60);
    let capacity = ProviderCapacity::from_windows(vec![RateLimitWindow {
        used_percentage: Some(20),
        resets_at: Some(reset),
        duration_mins: Some(10_080),
        observed_at: Some(now),
        ..Default::default()
    }]);
    let chain = |expiry| ResetCredits {
        count: 3,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry, expiry, expiry],
    };

    assert_eq!(
        redeem_verdict(
            Some(&capacity),
            &chain(reset + MIN_HOLD),
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            now,
        ),
        None,
        "a free refill wins while the credit retains a full hold interval"
    );
    assert_eq!(
        redeem_verdict(
            Some(&capacity),
            &chain(reset + MIN_HOLD - Duration::from_secs(1)),
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            now,
        ),
        Some(RedeemReason::ScheduledRedeem),
        "a credit that cannot survive the reset keeps its chain deadline"
    );
}

#[test]
fn spent_reasons_and_opt_out_take_precedence_over_chain_scheduling() {
    let now = ts(1_700_000_000);
    let expiry = now + Duration::from_secs(3 * 86_400);
    let chain = ResetCredits {
        count: 13,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry; 13],
    };
    let blocked = spent_capacity(now, Duration::from_secs(2 * 86_400));

    assert_eq!(
        redeem_verdict(
            Some(&blocked),
            &chain,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            now,
        ),
        Some(RedeemReason::BlockedGain),
    );
    let doomed_expiry = now + Duration::from_secs(12 * 3_600);
    let doomed = ResetCredits {
        count: 3,
        soonest_expiry: Some(doomed_expiry),
        expiries: vec![doomed_expiry; 3],
    };
    let short_block = spent_capacity(now, Duration::from_secs(60 * 60));
    assert_eq!(
        redeem_verdict(
            Some(&short_block),
            &doomed,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            now,
        ),
        Some(RedeemReason::DoomedCredit),
    );
    assert_eq!(
        redeem_verdict(
            None,
            &chain,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            false,
            now,
        ),
        None,
    );
}

#[test]
fn scheduled_reason_round_trips() {
    assert_eq!(RedeemReason::ScheduledRedeem.as_str(), "scheduled_redeem");
    assert_eq!(
        "scheduled_redeem".parse::<RedeemReason>().unwrap(),
        RedeemReason::ScheduledRedeem
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
        Default::default(),
        AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(100),
                resets_at: Some(now + Duration::from_secs(3 * 86_400)),
                duration_mins: Some(10_080),
                observed_at: Some(now),
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
    assert_eq!(
        read_rate_stamp(&runtime.shared_auto_redeem_rate_path(CODEX_KIND)),
        Some(RateStamp {
            window_resets_at: now + Duration::from_secs(3 * 86_400),
            last_used_pct: 100,
            last_observed_at: now,
            rate_pct_per_day: 0.0,
        })
    );

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
fn attempted_errors_retain_the_redeem_decision_report() {
    let report = RedeemReport {
        reason: RedeemReason::BlockedGain,
        credits: 2,
        soonest_expiry: Some(ts(300)),
        natural_reset: Some(ts(200)),
        outcome: None,
        windows_reset: false,
        window_resets: Vec::new(),
        reset: false,
    };

    let error = attempted_error(&report, AutoRedeemErr::Codex("offline".to_owned()));
    assert_eq!(error.attempted_report(), Some(&report));
    assert!(error.to_string().contains("offline"));
}

#[test]
fn failed_reservation_never_consumes_a_credit() {
    let dir = tempfile::tempdir().unwrap();
    let invalid_stamp_path = dir.path().join("parent-is-a-file").join("stamp.json");
    std::fs::write(dir.path().join("parent-is-a-file"), b"occupied").unwrap();
    let stamp = RedeemStamp {
        attempted_at: ts(1_700_000_000),
        request_id: "request".to_owned(),
        reason: RedeemReason::BlockedGain,
        outcome: None,
    };
    let report = RedeemReport {
        reason: RedeemReason::BlockedGain,
        credits: 1,
        soonest_expiry: None,
        natural_reset: None,
        outcome: None,
        windows_reset: false,
        window_resets: Vec::new(),
        reset: false,
    };
    let consumed = std::cell::Cell::new(false);

    let result = consume_reserved_reset_credit(
        &invalid_stamp_path,
        &stamp,
        &report,
        RedeemReason::BlockedGain,
        || {
            consumed.set(true);
            unreachable!("a failed durable reservation must stop the consume request")
        },
    );
    let Err(error) = result else {
        panic!("reservation must fail")
    };

    assert!(!consumed.get());
    assert_eq!(error.attempted_report(), Some(&report));
}
