use super::*;
use crate::agents::context::{AgentContext, AgentTurnError, TurnErrorClass};
use crate::agents::{AgentRateLimits, RateLimitWindow, RedeemEffect};
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

fn capacity_started_at(window_start: Timestamp, used_percentage: u8) -> ProviderCapacity {
    ProviderCapacity::from_windows(vec![RateLimitWindow {
        used_percentage: Some(used_percentage),
        resets_at: Some(window_start + Duration::from_secs(7 * 86_400)),
        duration_mins: Some(10_080),
        observed_at: Some(window_start),
        ..Default::default()
    }])
}

/// Opens the window half of the gate without a blocked gain: a spent 5h window
/// resetting two hours after `now`, nearer than the 12h `min_gain`.
fn with_spent_five_hour(mut capacity: ProviderCapacity, now: Timestamp) -> ProviderCapacity {
    capacity.windows.push(RateLimitWindow {
        used_percentage: Some(100),
        resets_at: Some(now + Duration::from_secs(2 * 3_600)),
        duration_mins: Some(300),
        observed_at: Some(now),
        ..Default::default()
    });
    capacity
}

fn keeping_schedule(credits: ResetCredits) -> ResetCredits {
    ResetCredits {
        effect: RedeemEffect::KeepsSchedule,
        ..credits
    }
}

fn credits(now: Timestamp, expiry: Option<Duration>) -> ResetCredits {
    ResetCredits {
        count: 1,
        soonest_expiry: expiry.map(|duration| now + duration),
        expiries: Vec::new(),
        effect: crate::agents::RedeemEffect::RestartsWindow,
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
        Some(RedeemReason::BlockedGain),
        "a restarting window never redeems as doomed: the 24h gain alone pays"
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
        None,
        "a 5h-only block waits even when the credit dies first"
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
                effect: crate::agents::RedeemEffect::RestartsWindow,
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
            true,
            now,
        ),
        None,
    );
    let dying = credits(now, Some(Duration::from_secs(20 * 60)));
    for dying in [dying.clone(), keeping_schedule(dying)] {
        assert_eq!(
            redeem_verdict(
                None,
                &dying,
                None,
                Duration::from_secs(12 * 3600),
                false,
                false,
                now,
            ),
            Some(RedeemReason::ExpiryRescue),
            "rescue needs neither the switch, a spent window, nor a paused agent ({:?})",
            dying.effect
        );
    }
}

#[test]
fn limit_redemption_waits_for_a_limit_paused_agent() {
    let now = ts(1_700_000_000);
    let blocked = spent_capacity(now, Duration::from_secs(3 * 86_400));
    let restarting = credits(now, Some(Duration::from_secs(10 * 86_400)));
    for credits in [restarting.clone(), keeping_schedule(restarting)] {
        for (limit_paused, expected) in [(false, None), (true, Some(RedeemReason::BlockedGain))] {
            assert_eq!(
                redeem_verdict(
                    Some(&blocked),
                    &credits,
                    None,
                    Duration::from_secs(12 * 3600),
                    true,
                    limit_paused,
                    now,
                ),
                expected,
                "limit_paused {limit_paused} ({:?})",
                credits.effect
            );
        }
    }
}

#[test]
fn a_kept_schedule_redeems_doomed_credits_and_never_schedules() {
    let now = ts(1_700_000_000);
    let hour = Duration::from_secs(3_600);
    let kept = |block: Duration, expiry: Duration| {
        verdict(
            Some(&spent_capacity(now, block)),
            &keeping_schedule(credits(now, Some(expiry))),
            now,
        )
    };
    assert_eq!(kept(24 * hour, 25 * hour), Some(RedeemReason::DoomedCredit));
    assert_eq!(kept(5 * hour, 4 * hour), Some(RedeemReason::DoomedCredit));
    assert_eq!(
        kept(3 * 24 * hour, 10 * 24 * hour),
        Some(RedeemReason::BlockedGain)
    );
    assert_eq!(kept(4 * hour, 40 * hour), None);

    let (capacity, chain) = due_chain(now);
    let schedule = |credits: &ResetCredits| {
        redeem_verdict(
            Some(&capacity),
            credits,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            now,
        )
    };
    assert_eq!(schedule(&chain), Some(RedeemReason::ScheduledRedeem));
    assert_eq!(schedule(&keeping_schedule(chain)), None);
}

/// A due chain behind an open gate: the 5h window is spent and resets in 4h
/// (under `min_gain`), the week is 40% used and resets in three days, and 13
/// credits expire with the week.
fn due_chain(now: Timestamp) -> (ProviderCapacity, ResetCredits) {
    let expiry = now + Duration::from_secs(3 * 86_400);
    let capacity = ProviderCapacity::from_windows(vec![
        RateLimitWindow {
            used_percentage: Some(100),
            resets_at: Some(now + Duration::from_secs(4 * 3_600)),
            duration_mins: Some(300),
            observed_at: Some(now),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(40),
            resets_at: Some(expiry),
            duration_mins: Some(10_080),
            observed_at: Some(now),
            ..Default::default()
        },
    ]);
    let chain = ResetCredits {
        count: 13,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry; 13],
        effect: RedeemEffect::RestartsWindow,
    };
    (capacity, chain)
}

#[test]
fn the_schedule_fires_only_behind_the_gate() {
    let now = ts(1_700_000_000);
    let (spent, chain) = due_chain(now);
    let mut unspent = spent.clone();
    unspent.windows[0].used_percentage = Some(40);
    let schedule = |capacity: &ProviderCapacity, limit_paused| {
        redeem_verdict(
            Some(capacity),
            &chain,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            limit_paused,
            now,
        )
    };
    assert_eq!(schedule(&unspent, true), None, "no spent window");
    assert_eq!(schedule(&spent, false), None, "no limit-paused agent");
    assert_eq!(schedule(&spent, true), Some(RedeemReason::ScheduledRedeem));
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

    let noisy = update_rate_stamp(
        Some(&first),
        &window(11, reset, observed + Duration::from_secs(5 * 60)),
    )
    .unwrap();
    assert_eq!(noisy, first, "a five-minute 1% tick is not a stable seed");

    let learned = update_rate_stamp(Some(&first), &window(15, reset, observed + T_MIN)).unwrap();
    assert_eq!(learned.rate_pct_per_day, 20.0);

    let folded = update_rate_stamp(
        Some(&learned),
        &window(25, reset, observed + T_MIN + Duration::from_secs(86_400)),
    )
    .unwrap();
    let alpha = 1.0 - 0.5_f64.powf(1.0 / 3.0);
    let expected = 20.0 + alpha * (10.0 - 20.0);
    assert!((folded.rate_pct_per_day - expected).abs() < 1e-9);

    let next_reset = reset + Duration::from_secs(86_400);
    let restarted = update_rate_stamp(
        Some(&folded),
        &window(
            1,
            next_reset,
            observed + T_MIN + Duration::from_secs(2 * 86_400),
        ),
    )
    .unwrap();
    assert_eq!(restarted.window_resets_at, next_reset);
    assert_eq!(restarted.last_used_pct, 1);
    assert_eq!(restarted.rate_pct_per_day, folded.rate_pct_per_day);

    let stale = update_rate_stamp(
        Some(&restarted),
        &window(
            90,
            next_reset,
            observed + T_MIN + Duration::from_secs(86_400),
        ),
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
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };
    let deadline = rescue - refill - refill;
    let capacity = capacity_started_at(deadline - refill, 80);
    let early = deadline - Duration::from_secs(1);
    assert_eq!(
        redeem_verdict(
            Some(&with_spent_five_hour(capacity.clone(), early)),
            &credits,
            Some(20.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            early,
        ),
        None,
    );
    assert_eq!(
        redeem_verdict(
            Some(&with_spent_five_hour(capacity, deadline)),
            &credits,
            Some(20.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            deadline,
        ),
        Some(RedeemReason::ScheduledRedeem),
    );
}

#[test]
fn expired_credit_does_not_suppress_the_live_chain() {
    let now = ts(1_700_000_000);
    let expiry = now + Duration::from_secs(20 * 86_400);
    let credits = ResetCredits {
        count: 2,
        soonest_expiry: Some(now - Duration::from_secs(1)),
        expiries: vec![now - Duration::from_secs(1), expiry, expiry],
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };
    let deadline = expiry - EXPIRY_RESCUE_LEAD - Duration::from_secs(5 * 86_400);
    let capacity = with_spent_five_hour(
        capacity_started_at(deadline - Duration::from_secs(5 * 86_400), 80),
        deadline,
    );

    assert_eq!(
        redeem_verdict(
            Some(&capacity),
            &credits,
            Some(20.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            deadline,
        ),
        Some(RedeemReason::ScheduledRedeem),
    );
}

#[test]
fn window_start_paces_a_late_chain_after_every_reset() {
    let now = ts(1_700_000_000);
    let expiry = now + Duration::from_secs(36 * 3_600);
    let credits = ResetCredits {
        count: 3,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry, expiry, expiry],
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };
    let fresh = capacity_started_at(now, 0);

    assert!(chain_deadline(&credits.expiries, Some(100.0)).unwrap() < now);
    let later = now + Duration::from_secs(24 * 3_600);
    assert_eq!(
        redeem_verdict(
            Some(&with_spent_five_hour(fresh.clone(), now)),
            &credits,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            now,
        ),
        None,
        "a freshly zeroed window must not spend the next credit immediately"
    );
    assert_eq!(
        redeem_verdict(
            Some(&with_spent_five_hour(fresh, later)),
            &credits,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            later,
        ),
        Some(RedeemReason::ScheduledRedeem),
    );
}

#[test]
fn slow_burn_does_not_drain_an_overdue_chain_after_the_cooldown() {
    let now = ts(1_700_000_000);
    let expiry = now + Duration::from_secs(30 * 86_400);
    let credits = ResetCredits {
        count: 2,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry, expiry],
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };
    let fresh = with_spent_five_hour(capacity_started_at(now, 0), now + POST_SUCCESS_COOLDOWN);

    assert!(chain_deadline(&credits.expiries, Some(2.0)).unwrap() < now);
    assert_eq!(
        redeem_verdict(
            Some(&fresh),
            &credits,
            Some(2.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            now + POST_SUCCESS_COOLDOWN,
        ),
        None,
    );
}

#[test]
fn scheduled_redeem_requires_a_dated_duration_window() {
    let now = ts(1_700_000_000);
    let expiry = now + Duration::from_secs(36 * 3_600);
    let credits = ResetCredits {
        count: 3,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry, expiry, expiry],
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };
    let missing_reset = ProviderCapacity::from_windows(vec![RateLimitWindow {
        used_percentage: Some(50),
        duration_mins: Some(10_080),
        ..Default::default()
    }]);
    let missing_duration = ProviderCapacity::from_windows(vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(7 * 86_400)),
        ..Default::default()
    }]);

    for capacity in [missing_reset, missing_duration] {
        assert_eq!(
            redeem_verdict(
                Some(&with_spent_five_hour(capacity, now)),
                &credits,
                Some(100.0),
                Duration::from_secs(12 * 3_600),
                true,
                true,
                now,
            ),
            None,
        );
    }
}

#[test]
fn near_free_reset_defers_only_a_credit_that_comfortably_survives() {
    let now = ts(1_700_000_000);
    let reset = now + Duration::from_secs(60 * 60);
    let capacity = with_spent_five_hour(
        ProviderCapacity::from_windows(vec![RateLimitWindow {
            used_percentage: Some(20),
            resets_at: Some(reset),
            duration_mins: Some(10_080),
            observed_at: Some(now),
            ..Default::default()
        }]),
        now,
    );
    let chain = |expiry| ResetCredits {
        count: 3,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry, expiry, expiry],
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };

    assert_eq!(
        redeem_verdict(
            Some(&capacity),
            &chain(reset + MIN_HOLD),
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
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
        effect: crate::agents::RedeemEffect::RestartsWindow,
    };
    let blocked = spent_capacity(now, Duration::from_secs(2 * 86_400));

    assert_eq!(
        redeem_verdict(
            Some(&blocked),
            &chain,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
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
        effect: RedeemEffect::KeepsSchedule,
    };
    let short_block = spent_capacity(now, Duration::from_secs(60 * 60));
    assert_eq!(
        redeem_verdict(
            Some(&short_block),
            &doomed,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            true,
            true,
            now,
        ),
        Some(RedeemReason::DoomedCredit),
    );
    assert_eq!(
        redeem_verdict(
            Some(&blocked),
            &chain,
            Some(100.0),
            Duration::from_secs(12 * 3_600),
            false,
            true,
            now,
        ),
        None,
    );
}

#[test]
fn scheduled_reason_round_trips() {
    assert_eq!(RedeemReason::ScheduledRedeem.as_str(), "scheduled_redeem");
    assert_eq!(
        serde_json::from_str::<RedeemReason>("\"scheduled_redeem\"").unwrap(),
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
    let path = runtime.shared_auto_redeem_path(&crate::ids::LoginKey::default_for(
        crate::ids::AgentKind::new_unchecked("codex"),
    ));
    let stamp = RedeemStamp {
        attempted_at: ts(1_700_000_000),
        request_id: "0195-request".to_owned(),
        reason: RedeemReason::DoomedCredit,
        outcome: Some("nothing_to_reset".to_owned()),
    };

    write_stamp(&path, &stamp).unwrap();

    assert_eq!(read_stamp(&path), Some(stamp));
}

fn manual_report() -> RedeemReport {
    RedeemReport {
        reason: RedeemReason::Manual,
        credits: 1,
        soonest_expiry: None,
        natural_reset: None,
        outcome: None,
        windows_reset: false,
        window_resets: Vec::new(),
    }
}

#[test]
fn manual_consume_refuses_held_offers_before_locking_or_reserving() {
    use crate::agents::account::{ResetCreditAction, ResetCreditOffer};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Action(Arc<AtomicUsize>);
    impl ResetCreditAction for Action {
        fn consume(self: Box<Self>, _: &str) -> Result<ResetCreditResult, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ResetCreditResult {
                outcome: RedemptionCode::Reset,
                windows_reset: 0,
                refreshed: None,
                refresh_error: None,
            })
        }
    }

    for code in [RedemptionCode::NoCredit, RedemptionCode::Cooldown] {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let key = LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude"));
        let consumed = Arc::new(AtomicUsize::new(0));
        let mut offer = ResetCreditOffer::new(
            None,
            credits(ts(1_700_000_000), None),
            Action(consumed.clone()),
        );
        let hold = RedeemHold {
            code,
            reason: "provider hold".to_owned(),
        };
        offer.hold = Some(hold.clone());
        let preview = ManualRedeem {
            credits: offer.credits.clone(),
            windows: Vec::new(),
            natural_reset: None,
            forecast: None,
            min_gain: Duration::ZERO,
            prepared: PreparedRedemption::from_offer(RedeemReason::Manual, offer),
            stamp_at_read: None,
        };
        let result = preview.consume(&runtime, &key, uuid::Uuid::now_v7());
        assert!(matches!(result, Err(AutoRedeemErr::Held(actual)) if actual == hold));
        assert_eq!(consumed.load(Ordering::SeqCst), 0);
        assert!(!runtime.shared_auto_redeem_lock(&key).exists());
        assert!(!runtime.shared_auto_redeem_path(&key).exists());
    }
}

#[test]
fn manual_tail_bypasses_cooldown_and_reserves_before_consuming() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let key = LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND));
    let path = runtime.shared_auto_redeem_path(&key);
    let now = ts(1_700_000_000);
    let seen = RedeemStamp {
        attempted_at: now - SignedDuration::from_secs(60),
        request_id: "auto".to_owned(),
        reason: RedeemReason::BlockedGain,
        outcome: Some("reset".to_owned()),
    };
    write_stamp(&path, &seen).unwrap();
    let consumed = std::cell::Cell::new(false);
    let redeemed = consume_manual_redemption(
        &runtime,
        &key,
        manual_report(),
        Some(&seen),
        now,
        "manual",
        || {
            let reservation = read_stamp(&path).unwrap();
            assert_eq!(reservation.reason, RedeemReason::Manual);
            assert_eq!(reservation.request_id, "manual");
            assert_eq!(reservation.outcome, None);
            consumed.set(true);
            Ok(ResetCreditResult {
                outcome: RedemptionCode::Reset,
                windows_reset: 2,
                refreshed: None,
                refresh_error: Some("refresh failed".to_owned()),
            })
        },
    );
    assert!(
        redeemed.is_ok(),
        "manual redemption must ignore the auto cooldown"
    );
    let redeemed = redeemed.unwrap();
    assert!(consumed.get());
    assert_eq!(redeemed.report.reason, RedeemReason::Manual);
    assert_eq!(redeemed.report.outcome, Some(RedemptionCode::Reset));
    assert!(redeemed.usage.is_none());
    assert_eq!(redeemed.refresh_error.as_deref(), Some("refresh failed"));
    let stamp = read_stamp(&path).unwrap();
    assert_eq!(stamp.reason, RedeemReason::Manual);
    assert_eq!(stamp.outcome.as_deref(), Some("reset"));
    assert!(!stamp_allows_attempt(Some(&stamp), now + ATTEMPT_COOLDOWN));
}

#[test]
fn manual_tail_refuses_newer_attempts_and_live_reservations() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let key = LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND));
    let path = runtime.shared_auto_redeem_path(&key);
    let now = ts(1_700_000_000);
    let reservation = RedeemStamp {
        attempted_at: now - SignedDuration::from_secs(60),
        outcome: None,
        request_id: "auto".to_owned(),
        reason: RedeemReason::BlockedGain,
    };
    // An attempt that landed after the preview, one already in flight at the
    // preview that finished since, and a reservation the preview itself saw.
    for (seen, attempted_at, outcome) in [
        (None, now + SignedDuration::from_secs(1), Some("reset")),
        (None, now - SignedDuration::from_secs(5), Some("reset")),
        (Some(&reservation), reservation.attempted_at, None),
    ] {
        let stamp = RedeemStamp {
            attempted_at,
            outcome: outcome.map(str::to_owned),
            request_id: "auto".to_owned(),
            reason: RedeemReason::BlockedGain,
        };
        write_stamp(&path, &stamp).unwrap();
        let result = consume_manual_redemption(
            &runtime,
            &key,
            manual_report(),
            seen,
            now + Duration::from_secs(2),
            "manual",
            || panic!("a racing helper must prevent consuming"),
        );
        let error = result.err().expect("must refuse").to_string();
        assert!(
            error.contains("auto-redeem") && error.contains("rerun"),
            "{error}"
        );
        assert_eq!(read_stamp(&path), Some(stamp));
    }
}

#[test]
fn manual_failed_reservation_never_consumes_a_credit() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let key = LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND));
    std::fs::create_dir_all(runtime.shared_auto_redeem_path(&key)).unwrap();
    let result = consume_manual_redemption(
        &runtime,
        &key,
        manual_report(),
        None,
        ts(10),
        "manual",
        || panic!("a failed reservation must prevent consuming"),
    );
    let error = result.err().expect("must fail");
    assert_eq!(error.attempted_report(), Some(&manual_report()));
    assert!(error.to_string().contains("stamp"));
}

#[test]
fn manual_stamp_reads_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stamp.json");
    std::fs::write(&path, br#"{"attempted_at":"2026-01-01T00:00:00Z","request_id":"manual","reason":"manual","outcome":"reset"}"#).unwrap();
    let stamp = read_stamp(&path).expect("manual stamp must parse");
    assert_eq!(stamp.reason, RedeemReason::Manual);
    write_stamp(&path, &stamp).unwrap();
    assert_eq!(read_stamp(&path), Some(stamp));
}

/// A live root row whose displayed turn error, raised at `now`, has `class`.
fn limit_parked(kind: &str, agent_id: &str, class: TurnErrorClass, now: Timestamp) -> AgentState {
    let mut agent = crate::sidebar::test_support::root_agent(kind, agent_id, None);
    agent.last_activity = now - Duration::from_secs(60);
    agent.context = Some(AgentContext {
        turn_error: Some(AgentTurnError {
            class,
            at: now,
            label: None,
        }),
        ..Default::default()
    });
    agent
}

#[test]
fn a_request_round_trips_its_paused_evidence_and_defaults_to_not_paused() {
    let request = AutoRedeemRequest {
        workspace_id: WorkspaceId::from_project_root(Path::new("/srv/project")),
        login: LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND)),
        reason: RedeemReason::BlockedGain,
        request_id: uuid::Uuid::now_v7(),
        limit_paused: true,
    };
    let mut payload = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<AutoRedeemRequest>(payload.clone()).unwrap(),
        request
    );
    payload.as_object_mut().unwrap().remove("limit_paused");
    let older = serde_json::from_value::<AutoRedeemRequest>(payload).unwrap();
    assert!(!older.limit_paused);
}

#[test]
fn producer_reserves_a_spawn_and_paces_the_next_tick() {
    let now = ts(1_700_000_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let mut cache = crate::agents::account::RateLimitsCache::default();
    cache.entries.insert(
        LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND)),
        crate::agents::account::RateLimitCacheEntry {
            limits: AgentRateLimits {
                windows: vec![RateLimitWindow {
                    used_percentage: Some(100),
                    resets_at: Some(now + Duration::from_secs(3 * 86_400)),
                    duration_mins: Some(10_080),
                    observed_at: Some(now),
                    source: crate::agents::context::WindowSource::Authoritative,
                    ..Default::default()
                }],
            },
            ..Default::default()
        },
    );
    write_temp_then_rename_cache(&runtime.shared_rate_limits_path(), &cache).unwrap();
    let mut panel = crate::sidebar::test_support::provider_panel(CODEX_KIND, Vec::new());
    panel.reset_credits = Some(credits(now, Some(Duration::from_secs(10 * 86_400))));
    let config = ResumeConfig {
        auto_redeem: true,
        ..Default::default()
    };
    let stamp_path = runtime.shared_auto_redeem_path(&crate::ids::LoginKey::default_for(
        crate::ids::AgentKind::new_unchecked(CODEX_KIND),
    ));

    let mut other_login = limit_parked(CODEX_KIND, "work", TurnErrorClass::PausedRateLimit, now);
    other_login.login = Some("work".parse().unwrap());
    let mut budget_parked =
        limit_parked(CODEX_KIND, "budget", TurnErrorClass::PausedSpendLimit, now);
    budget_parked.budget_park = Some(crate::agents::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::agents::BudgetWindow::Day,
        at: now,
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
    });
    let mut subagent = limit_parked(CODEX_KIND, "sub", TurnErrorClass::PausedRateLimit, now);
    subagent.parent_agent_id = Some("root".into());
    let mut ended = limit_parked(CODEX_KIND, "ended", TurnErrorClass::PausedRateLimit, now);
    ended.ended_at = Some(now);
    let closed = [
        other_login,
        limit_parked("claude", "claude", TurnErrorClass::PausedRateLimit, now),
        budget_parked,
        limit_parked(CODEX_KIND, "busy", TurnErrorClass::PausedOverloaded, now),
        subagent,
        ended,
    ];
    redeem_credits(
        std::slice::from_ref(&panel),
        &BTreeMap::new(),
        &closed,
        &runtime,
        &crate::agents::RoomLoginSet::native(),
        &config,
        now,
    );
    assert_eq!(
        read_stamp(&stamp_path),
        None,
        "no row of this room is stopped on the Codex default login's limit"
    );
    assert!(
        read_rate_stamp(
            &runtime.shared_auto_redeem_rate_path(&crate::ids::LoginKey::default_for(
                crate::ids::AgentKind::new_unchecked(CODEX_KIND)
            ))
        )
        .is_some(),
        "the burn-rate cache advances whatever the gate says"
    );

    let mut open = closed.to_vec();
    open.push(limit_parked(
        CODEX_KIND,
        "root",
        TurnErrorClass::PausedRateLimit,
        now,
    ));
    redeem_credits(
        std::slice::from_ref(&panel),
        &BTreeMap::new(),
        &open,
        &runtime,
        &crate::agents::RoomLoginSet::native(),
        &config,
        now,
    );
    let first = read_stamp(&stamp_path).unwrap();
    assert_eq!(first.attempted_at, now);
    assert_eq!(first.reason, RedeemReason::BlockedGain);
    assert_eq!(first.outcome, None);
    assert_eq!(
        read_rate_stamp(
            &runtime.shared_auto_redeem_rate_path(&crate::ids::LoginKey::default_for(
                crate::ids::AgentKind::new_unchecked(CODEX_KIND)
            ))
        ),
        Some(RateStamp {
            window_resets_at: now + Duration::from_secs(3 * 86_400),
            last_used_pct: 100,
            last_observed_at: now,
            rate_pct_per_day: 0.0,
        })
    );

    redeem_credits(
        std::slice::from_ref(&panel),
        &BTreeMap::new(),
        &open,
        &runtime,
        &crate::agents::RoomLoginSet::native(),
        &config,
        now + Duration::from_secs(1),
    );
    assert_eq!(
        read_stamp(
            &runtime.shared_auto_redeem_path(&crate::ids::LoginKey::default_for(
                crate::ids::AgentKind::new_unchecked(CODEX_KIND)
            ))
        ),
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
    let path = runtime.shared_auto_redeem_path(&crate::ids::LoginKey::default_for(
        crate::ids::AgentKind::new_unchecked(CODEX_KIND),
    ));

    assert!(reserve_attempt(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND)),
        RedeemReason::ExpiryRescue,
        now,
        "request-a"
    ));
    cancel_attempt_reservation(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND)),
        "request-b",
    );
    assert_eq!(read_stamp(&path).unwrap().request_id, "request-a");

    cancel_attempt_reservation(
        &runtime,
        &crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND)),
        "request-a",
    );
    assert!(read_stamp(&path).is_none());
}

fn rate_limit_entry(
    windows: &[(u32, u8)],
    now: Timestamp,
) -> crate::agents::account::RateLimitCacheEntry {
    crate::agents::account::RateLimitCacheEntry {
        limits: AgentRateLimits {
            windows: windows
                .iter()
                .map(|(duration_mins, used_percentage)| RateLimitWindow {
                    used_percentage: Some(*used_percentage),
                    resets_at: Some(now + Duration::from_secs(3 * 86_400)),
                    duration_mins: Some(*duration_mins),
                    observed_at: Some(now),
                    source: crate::agents::context::WindowSource::Authoritative,
                    ..Default::default()
                })
                .collect(),
        },
        ..Default::default()
    }
}

#[test]
fn an_idle_account_spawns_a_rescue_only_at_expiry_whatever_its_cached_windows_read() {
    let now = ts(1_700_000_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let key = |name: &str| format!("codex@{name}").parse::<LoginKey>().unwrap();
    let default = LoginKey::default_for(crate::ids::AgentKind::new_unchecked(CODEX_KIND));
    let mut cache = crate::agents::account::RateLimitsCache::default();
    cache.entries.extend([
        (
            default.clone(),
            rate_limit_entry(&[(300, 0), (10_080, 0)], now),
        ),
        (
            key("used"),
            rate_limit_entry(&[(300, 0), (10_080, 40)], now),
        ),
        (key("spent"), rate_limit_entry(&[(10_080, 100)], now)),
        (
            key("unused"),
            rate_limit_entry(&[(300, 0), (10_080, 0)], now),
        ),
    ]);
    // A fresh Codex 5h window reads 1% with its reset a full window out; the
    // same 1% on a running clock is usage.
    for (name, five_hour_reset) in [("fresh", 300 * 60), ("running", 2 * 3_600)] {
        let mut entry = rate_limit_entry(&[(300, 1), (10_080, 0)], now);
        entry.limits.windows[0].resets_at = Some(now + Duration::from_secs(five_hour_reset));
        cache.entries.insert(key(name), entry);
    }
    write_temp_then_rename_cache(&runtime.shared_rate_limits_path(), &cache).unwrap();
    let dying = credits(now, Some(Duration::from_secs(20 * 60)));
    let idle_credits = BTreeMap::from([
        (key("fresh"), dying.clone()),
        (key("running"), dying.clone()),
        (key("used"), dying.clone()),
        (
            key("spent"),
            credits(now, Some(Duration::from_secs(10 * 86_400))),
        ),
        (key("unused"), dying.clone()),
        (key("unknown"), dying.clone()),
    ]);
    let mut panel = crate::sidebar::test_support::provider_panel(CODEX_KIND, Vec::new());
    panel.reset_credits = Some(dying);
    let config = ResumeConfig {
        auto_redeem: true,
        ..Default::default()
    };

    let mut spawned = Vec::new();
    redeem_credits_with(
        std::slice::from_ref(&panel),
        &idle_credits,
        &[],
        &runtime,
        &crate::agents::RoomLoginSet::native(),
        &config,
        now,
        |_, key, reason, _, limit_paused| {
            spawned.push((key.to_string(), reason, limit_paused));
            true
        },
    );

    let rescue = |name: &str| (format!("codex@{name}"), RedeemReason::ExpiryRescue, false);
    assert_eq!(
        spawned,
        [
            rescue("default"),
            rescue("fresh"),
            rescue("running"),
            rescue("unknown"),
            rescue("unused"),
            rescue("used")
        ],
        "a dying credit spawns the helper whatever the cached windows read, since the \
         unused-window skip is the helper's on its fresh read; a spent window alone \
         redeems nothing"
    );
    assert!(read_stamp(&runtime.shared_auto_redeem_path(&key("unused"))).is_some());
    assert!(read_stamp(&runtime.shared_auto_redeem_path(&key("spent"))).is_none());
    assert!(read_rate_stamp(&runtime.shared_auto_redeem_rate_path(&default)).is_some());
    assert!(
        read_rate_stamp(&runtime.shared_auto_redeem_rate_path(&key("used"))).is_none(),
        "burn-rate sampling stays with in-use logins"
    );
}

#[test]
fn the_helper_judges_an_idle_login_as_rescue_only_whatever_the_request_carried() {
    let now = ts(1_700_000_000);
    let config = ResumeConfig {
        auto_redeem: true,
        ..Default::default()
    };
    let blocked = spent_capacity(now, Duration::from_secs(3 * 86_400));
    let held = credits(now, Some(Duration::from_secs(10 * 86_400)));
    let dying = credits(now, Some(Duration::from_secs(20 * 60)));
    let fresh = |idle, capacity: Option<&ProviderCapacity>, credits: &ResetCredits| {
        fresh_verdict(idle, capacity, credits, None, &config, true, now)
    };
    assert_eq!(
        fresh(false, Some(&blocked), &held),
        Some(RedeemReason::BlockedGain)
    );
    assert_eq!(fresh(true, Some(&blocked), &held), None);
    assert_eq!(
        fresh(true, Some(&blocked), &dying),
        Some(RedeemReason::ExpiryRescue)
    );
    assert_eq!(fresh(true, None, &dying), Some(RedeemReason::ExpiryRescue));
    let unused = capacity_started_at(now, 0);
    assert_eq!(fresh(true, Some(&unused), &dying), None);
    let five_hour = |used_percentage, reset_secs| RateLimitWindow {
        used_percentage: Some(used_percentage),
        resets_at: Some(now + Duration::from_secs(reset_secs)),
        duration_mins: Some(300),
        observed_at: Some(now),
        ..Default::default()
    };
    let mut not_started = unused.clone();
    not_started.windows.push(five_hour(1, 300 * 60));
    assert_eq!(fresh(true, Some(&not_started), &dying), None);
    let mut running = unused.clone();
    running.windows.push(five_hour(1, 2 * 3_600));
    assert_eq!(
        fresh(true, Some(&running), &dying),
        Some(RedeemReason::ExpiryRescue)
    );
    assert_eq!(
        fresh(false, Some(&unused), &dying),
        Some(RedeemReason::ExpiryRescue)
    );
}

#[test]
fn redeem_keeps_a_live_old_login_continues_idle_once_it_ends_and_cancels_an_undeclared_key() {
    let now = ts(1_700_000_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let key: LoginKey = "codex@work".parse().unwrap();
    let accounts = toml::from_str("[codex.work]\nhome = '/srv/work'\n").unwrap();
    let catalog = crate::agents::LoginCatalog::from_config(&accounts).unwrap();
    let mut agent = crate::agents::AgentState::seed(
        key.kind.clone(),
        "root".into(),
        crate::agents::AgentStatus::Idle,
        now,
    );
    agent.login = Some(key.name.clone());
    let logins = crate::agents::RoomLoginSet::new(
        Some(Default::default()),
        Some(catalog),
        Default::default(),
    );
    assert!(reserve_attempt(
        &runtime,
        &key,
        RedeemReason::ExpiryRescue,
        now,
        "request"
    ));
    let in_use = logins.clone().with_agents(&[agent.clone()]);
    let (selected, idle) = redeem_login(&runtime, &key, "request", &in_use).unwrap();
    assert_eq!((selected.key(), idle), (key.clone(), false));
    assert!(idle_logins(&in_use).is_empty());
    // A room whose record cannot be read knows no default of its own, so
    // every declared login, `default` included, is idle: rescue only.
    let unreadable = crate::agents::RoomLoginSet::new(
        None,
        Some(crate::agents::LoginCatalog::from_config(&accounts).unwrap()),
        Default::default(),
    );
    assert_eq!(
        idle_logins(&unreadable)
            .iter()
            .map(|login| login.key().to_string())
            .collect::<Vec<_>>(),
        ["codex@default", "codex@work"]
    );
    let (_, idle) = redeem_login(&runtime, &key, "request", &unreadable).unwrap();
    assert!(idle);

    agent.ended_at = Some(now);
    let ended = logins.with_agents(&[agent]);
    assert_eq!(
        idle_logins(&ended)
            .iter()
            .map(|login| login.key())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&key)
    );
    let (selected, idle) = redeem_login(&runtime, &key, "request", &ended)
        .expect("a declared login no longer in use continues as idle");
    assert_eq!((selected.key(), idle), (key.clone(), true));
    assert_eq!(
        ended.env(&selected).get("CODEX_HOME").map(String::as_str),
        Some("/srv/work")
    );
    assert!(read_stamp(&runtime.shared_auto_redeem_path(&key)).is_some());

    let gone: LoginKey = "codex@gone".parse().unwrap();
    assert!(reserve_attempt(
        &runtime,
        &gone,
        RedeemReason::ExpiryRescue,
        now,
        "request"
    ));
    assert!(redeem_login(&runtime, &gone, "request", &ended).is_none());
    assert!(read_stamp(&runtime.shared_auto_redeem_path(&gone)).is_none());
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
    };

    let error = attempted_error(&report, AutoRedeemErr::Provider("offline".to_owned()));
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
    };
    let consumed = std::cell::Cell::new(false);

    let result = consume_reserved_reset_credit(
        &"codex@default".parse().unwrap(),
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

fn open_capacity(now: Timestamp, week_reset: Duration) -> ProviderCapacity {
    ProviderCapacity::from_windows(vec![
        RateLimitWindow {
            used_percentage: Some(40),
            resets_at: Some(now + Duration::from_secs(2 * 3_600)),
            duration_mins: Some(300),
            observed_at: Some(now),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(40),
            resets_at: Some(now + week_reset),
            duration_mins: Some(10_080),
            observed_at: Some(now),
            ..Default::default()
        },
    ])
}

fn chain(expiry: Timestamp) -> ResetCredits {
    ResetCredits {
        count: 2,
        soonest_expiry: Some(expiry),
        expiries: vec![expiry, expiry + Duration::from_secs(86_400)],
        effect: crate::agents::RedeemEffect::RestartsWindow,
    }
}

fn forecast(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    rate_pct_per_day: Option<f64>,
    auto_redeem: bool,
    now: Timestamp,
) -> Option<RedeemForecast> {
    redeem_forecast(
        capacity,
        credits,
        rate_pct_per_day,
        Duration::from_secs(12 * 3_600),
        auto_redeem,
        now,
    )
}

#[test]
fn forecast_reads_manual_armed_and_holding_from_a_dry_longest_window() {
    let now = ts(1_700_000_000);
    let day = Duration::from_secs(86_400);
    let hour = Duration::from_secs(3_600);

    let far = open_capacity(now, 3 * day);
    let blocked = chain(now + 5 * day);
    assert_eq!(
        forecast(Some(&far), &blocked, None, false, now),
        Some(RedeemForecast::Manual),
        "auto-redeem off stays manual even when a dry week would block the gain"
    );
    assert_eq!(
        forecast(Some(&far), &blocked, None, true, now),
        Some(RedeemForecast::Armed),
        "a dry week three days from reset redeems for the blocked gain"
    );

    let near = open_capacity(now, 7 * hour);
    assert_eq!(
        forecast(Some(&near), &chain(now + 19 * hour), None, true, now),
        Some(RedeemForecast::Holding),
        "a restarting window holds a credit expiring 12h after a near reset"
    );
    let survivor = chain(now + 7 * hour + 3 * day);
    for rate in [None, Some(20.0)] {
        assert_eq!(
            forecast(Some(&near), &survivor, rate, true, now),
            Some(RedeemForecast::Holding),
            "a near reset and a surviving credit hold at rate {rate:?}"
        );
    }
    assert_eq!(
        forecast(
            Some(&near),
            &chain(now + Duration::from_secs(20 * 60)),
            None,
            true,
            now
        ),
        Some(RedeemForecast::Armed),
        "the expiry rescue fires under auto-redeem too"
    );
}

#[test]
fn forecast_without_a_known_reset_is_armed_and_without_credits_is_none() {
    let now = ts(1_700_000_000);
    let credit = chain(now + Duration::from_secs(3 * 86_400));
    assert_eq!(
        forecast(None, &credit, None, true, now),
        Some(RedeemForecast::Armed)
    );
    let unstarted = ProviderCapacity::from_windows(vec![RateLimitWindow {
        used_percentage: Some(0),
        resets_at: None,
        duration_mins: Some(10_080),
        ..Default::default()
    }]);
    assert_eq!(
        forecast(Some(&unstarted), &credit, None, true, now),
        Some(RedeemForecast::Armed)
    );
    let empty = ResetCredits {
        count: 0,
        ..credit.clone()
    };
    assert_eq!(forecast(None, &empty, None, true, now), None);
    assert_eq!(forecast(None, &empty, None, false, now), None);
}

#[test]
fn holding_forecast_stays_firm_until_the_longest_reset() {
    let start = ts(1_700_000_000);
    let hour = Duration::from_secs(3_600);
    let day = Duration::from_secs(86_400);
    let open = (open_capacity(start, 7 * hour), start + 7 * hour);
    let spent_five_hour = (
        ProviderCapacity::from_windows(vec![
            RateLimitWindow {
                used_percentage: Some(100),
                resets_at: Some(start + 4 * hour),
                duration_mins: Some(300),
                observed_at: Some(start),
                ..Default::default()
            },
            RateLimitWindow {
                used_percentage: Some(40),
                resets_at: Some(start + 3 * hour),
                duration_mins: Some(10_080),
                observed_at: Some(start),
                ..Default::default()
            },
        ]),
        start + 3 * hour,
    );

    for (capacity, week_reset) in [open, spent_five_hour] {
        let credits = chain(week_reset + 3 * day);
        for rate in [None, Some(20.0)] {
            let mut now = start;
            while now < week_reset {
                assert_eq!(
                    forecast(Some(&capacity), &credits, rate, true, now),
                    Some(RedeemForecast::Holding),
                    "hold flipped at {now} (rate {rate:?})"
                );
                assert_eq!(
                    redeem_verdict(
                        Some(&capacity),
                        &credits,
                        rate,
                        Duration::from_secs(12 * 3_600),
                        true,
                        true,
                        now,
                    ),
                    None,
                    "the producer fired during a hold at {now} (rate {rate:?})"
                );
                now += Duration::from_secs(10 * 60);
            }
        }
    }
}

#[test]
fn projection_forecasts_only_the_codex_panel_with_credits() {
    let now = ts(1_700_000_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let mut snapshot =
        SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), Vec::new(), now);
    let mut codex = crate::sidebar::test_support::provider_panel(CODEX_KIND, Vec::new());
    codex.reset_credits = Some(chain(now + Duration::from_secs(3 * 86_400)));
    let mut other = crate::sidebar::test_support::provider_panel("claude", Vec::new());
    other.reset_credits = codex.reset_credits.clone();
    snapshot.providers = vec![codex, other];
    let logins = crate::agents::RoomLoginSet::native();
    let forecasts = |snapshot: &SidebarSnapshot| {
        snapshot
            .providers
            .iter()
            .map(|panel| panel.redeem_forecast)
            .collect::<Vec<_>>()
    };

    let mut config = ResumeConfig {
        auto_redeem: true,
        ..Default::default()
    };
    project_redeem_forecasts(&mut snapshot, &runtime, &config, &logins);
    assert_eq!(forecasts(&snapshot), [Some(RedeemForecast::Armed), None]);

    config.auto_redeem = false;
    project_redeem_forecasts(&mut snapshot, &runtime, &config, &logins);
    assert_eq!(forecasts(&snapshot), [Some(RedeemForecast::Manual), None]);

    snapshot.providers[0].reset_credits = None;
    project_redeem_forecasts(&mut snapshot, &runtime, &config, &logins);
    assert_eq!(forecasts(&snapshot), [None, None]);
}

#[test]
fn claude_banked_credits_never_arm_the_automatic_path() {
    let now = ts(1_700_000_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let mut panel = crate::sidebar::test_support::provider_panel("claude", Vec::new());
    panel.reset_credits = Some(keeping_schedule(credits(
        now,
        Some(Duration::from_secs(60)),
    )));
    let mut spawned = Vec::new();
    redeem_credits_with(
        &[panel],
        &BTreeMap::new(),
        &[],
        &runtime,
        &RoomLoginSet::native(),
        &ResumeConfig {
            auto_redeem: true,
            ..Default::default()
        },
        now,
        |_, key, _, _, _| {
            spawned.push(key.clone());
            true
        },
    );
    assert!(spawned.is_empty(), "{spawned:?}");
    let key = "claude@default".parse().unwrap();
    assert!(!runtime.shared_auto_redeem_path(&key).exists());
}
