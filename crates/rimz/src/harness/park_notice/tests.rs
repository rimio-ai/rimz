use std::path::PathBuf;
use std::time::Duration;

use super::*;
use crate::agents::{AgentContext, AgentTurnError, PermissionMode, TurnErrorClass};
use crate::store::run::RunStatus;

fn parked(class: TurnErrorClass, label: Option<&str>) -> (Vec<AgentState>, RunRecord) {
    let parent = AgentState::stub("claude", "parent", AgentStatus::Sleeping);
    let mut child = AgentState::stub("codex", "child", AgentStatus::Running);
    child.name = Some("still-silver".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    mark(&mut child, class, label);
    let mut run = RunRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/park-notice")),
        child.kind.clone(),
        PermissionMode::Auto,
        "gate".to_owned(),
        PathBuf::from("/tmp/park-notice"),
    );
    run.subagent = true;
    run.status = RunStatus::Running;
    run.agent_id = Some(child.agent_id.clone());
    (vec![parent, child], run)
}

/// A turn error one second after the child's last activity.
fn mark(child: &mut AgentState, class: TurnErrorClass, label: Option<&str>) {
    let at = child.last_activity + Duration::from_secs(1);
    let mut context = AgentContext::new(child.kind.as_str(), at);
    context.turn_error = Some(AgentTurnError {
        ended_turn: false,
        class,
        at,
        label: label.map(str::to_owned),
    });
    child.context = Some(context);
}

fn eligible(run: &RunRecord, agents: &[AgentState]) -> bool {
    unnoticed_park(run, agents, agents[0].last_activity).is_some()
}

#[test]
fn a_limit_park_is_classified_once_until_the_child_acts_again() {
    for class in [
        TurnErrorClass::PausedRateLimit,
        TurnErrorClass::PausedSpendLimit,
    ] {
        let (mut agents, mut run) = parked(class, Some("Usage limit reached"));
        let park = unnoticed_park(&run, &agents, agents[0].last_activity)
            .expect("a fresh park is classified");
        assert_eq!(park.child.agent_id.as_str(), "child");
        assert_eq!(park.parent.agent_id.as_str(), "parent");

        run.park_noticed_activity = Some(agents[1].last_activity);
        assert!(!eligible(&run, &agents), "this park is classified already");

        agents[1].last_activity += Duration::from_secs(600);
        assert!(
            !eligible(&run, &agents),
            "the old marker predates the resume"
        );
        mark(&mut agents[1], class, None);
        assert!(eligible(&run, &agents), "a second park after a resume");
    }
}

#[test]
fn only_a_live_limit_park_with_a_live_parent_is_classified() {
    let fresh = || parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
    let (agents, run) = fresh();
    assert!(eligible(&run, &agents));

    for class in [
        TurnErrorClass::PausedOverloaded,
        TurnErrorClass::Unknown,
        TurnErrorClass::Failed,
    ] {
        let (agents, run) = parked(class, Some("API Error: Overloaded"));
        assert!(!eligible(&run, &agents), "{class:?}");
    }
    for status in [
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Canceled,
        RunStatus::TimedOut,
    ] {
        let (agents, mut run) = fresh();
        run.status = status;
        assert!(!eligible(&run, &agents), "{status:?}");
    }
    let (agents, mut run) = fresh();
    run.subagent = false;
    assert!(!eligible(&run, &agents), "not a subagent run");
    let (agents, mut run) = fresh();
    run.report_to = ReportTo::Nobody;
    assert!(!eligible(&run, &agents), "a detached run reports to nobody");
    let (agents, mut run) = fresh();
    run.agent_id = None;
    assert!(!eligible(&run, &agents), "no session yet");
    let (mut agents, run) = fresh();
    agents[1].ended_at = Some(Timestamp::now());
    assert!(!eligible(&run, &agents), "ended child");
    let (mut agents, run) = fresh();
    agents[1].status = AgentStatus::Failed;
    agents[1].turn_started_at = Some(agents[1].last_activity);
    assert!(
        !eligible(&run, &agents),
        "a failed row is settled, not parked"
    );
    let (mut agents, run) = fresh();
    agents[0].ended_at = Some(Timestamp::now());
    assert!(!eligible(&run, &agents), "ended parent");
    let (mut agents, run) = fresh();
    agents.remove(0);
    assert!(!eligible(&run, &agents), "missing parent");
}

const HOUR: Duration = Duration::from_secs(3600);

fn hour() -> Duration {
    HOUR
}

#[test]
fn the_horizon_is_the_keep_warm_cap_whatever_keep_warm_says() {
    let mut harness = crate::config::HarnessConfig::default();
    assert_eq!(horizon(&harness), 6 * HOUR, "the default cap");
    harness.cache_keepalive_max = Some(2 * HOUR);
    assert_eq!(horizon(&harness), 2 * HOUR);
    harness.cache_keepalive = false;
    assert_eq!(horizon(&harness), 2 * HOUR, "pings off keep the cap");
    harness.cache_keepalive_max = None;
    assert_eq!(horizon(&harness), 6 * HOUR, "`off` holds up to 6h");
}

/// The child's marker time and the instant `secs` after it.
fn marker_plus(agents: &[AgentState], secs: u64) -> (Timestamp, Timestamp) {
    let at = agents[1]
        .context
        .as_ref()
        .unwrap()
        .turn_error
        .as_ref()
        .unwrap()
        .at;
    (at, at + Duration::from_secs(secs))
}

fn action<'a>(
    run: &RunRecord,
    agents: &'a [AgentState],
    now: Timestamp,
    continuation: Continuation,
    reading: LimitReading,
) -> Option<ParkAction<'a>> {
    park_action(run, agents, now, 6 * HOUR, continuation, |_, _| reading)
}

fn failed(action: Option<ParkAction<'_>>) -> Option<LimitFailure> {
    match action {
        Some(ParkAction::Fail { failure, .. }) => Some(failure),
        _ => None,
    }
}

#[test]
fn a_fresh_park_follows_the_seven_rules_in_order() {
    use Continuation::{Off, On, Spent};
    use LimitFailureCause as Cause;
    use LimitReading::{Refill, Refilled, Unknown};

    let since: Timestamp = "2026-01-01T12:00:00Z".parse().unwrap();
    let horizon = 6 * HOUR;
    let early = since + Duration::from_secs(30);
    let before = since + FAIL_WINDOW - Duration::from_secs(1);
    let mark = since + FAIL_WINDOW;
    let fail = |cause, refill| Classification::Fail(cause, refill);
    for now in [early, before, mark] {
        let far = now + horizon + Duration::from_secs(1);
        for continuation in [Off, On, Spent] {
            assert_eq!(
                classify(now, since, Refill(far), horizon, continuation),
                fail(Cause::BeyondHoldLimit, Some(far)),
                "rule 1 fails a refill past the hold limit at once"
            );
        }
        for near in [now + Duration::from_secs(1), now + horizon] {
            assert_eq!(
                classify(now, since, Refill(near), horizon, On),
                Classification::Hold { until: near },
                "rule 2 holds a near refill at once"
            );
        }
    }
    let near = |now: Timestamp| now + HOUR;
    for now in [early, before] {
        for (continuation, reading) in [
            (Off, Refill(near(now))),
            (Spent, Refill(near(now))),
            (Off, Refilled),
            (On, Refilled),
            (Off, Unknown),
            (On, Unknown),
            (Spent, Unknown),
        ] {
            assert_eq!(
                classify(now, since, reading, horizon, continuation),
                Classification::Wait,
                "rule 3 waits out the window: {continuation:?} {reading:?}"
            );
        }
    }
    for (continuation, reading, expected) in [
        (Spent, Refill(near(mark)), fail(Cause::ContinueFailed, None)),
        (Spent, Refilled, fail(Cause::ContinueFailed, None)),
        (Spent, Unknown, fail(Cause::ContinueFailed, None)),
        (
            Off,
            Refill(near(mark)),
            fail(Cause::RefillKnown, Some(near(mark))),
        ),
        (Off, Refilled, fail(Cause::Refilled, None)),
        (Off, Unknown, fail(Cause::RefillUnknown, None)),
        (On, Refilled, fail(Cause::RefillUnknown, None)),
        (On, Unknown, fail(Cause::RefillUnknown, None)),
    ] {
        assert_eq!(
            classify(mark, since, reading, horizon, continuation),
            expected,
            "at the mark: {continuation:?} {reading:?}"
        );
    }
}

#[test]
fn a_park_fails_after_the_window_with_its_label_and_refill() {
    let (agents, run) = parked(TurnErrorClass::PausedSpendLimit, Some("Spend cap"));
    let (marker, now) = marker_plus(&agents, 30);
    let far = now + 7 * HOUR;
    assert_eq!(
        failed(action(
            &run,
            &agents,
            now,
            Continuation::On,
            LimitReading::Refill(far)
        )),
        Some(LimitFailure {
            cause: LimitFailureCause::BeyondHoldLimit,
            refill_at: Some(far),
            label: Some("Spend cap".to_owned()),
        })
    );
    assert!(action(&run, &agents, now, Continuation::Off, LimitReading::Unknown).is_none());
    let mark = marker + FAIL_WINDOW;
    let Some(ParkAction::Fail {
        activity, failure, ..
    }) = action(
        &run,
        &agents,
        mark,
        Continuation::Off,
        LimitReading::Unknown,
    )
    else {
        panic!("the window has passed");
    };
    assert_eq!(activity, agents[1].last_activity);
    assert_eq!(
        failure,
        LimitFailure {
            cause: LimitFailureCause::RefillUnknown,
            refill_at: None,
            label: Some("Spend cap".to_owned()),
        }
    );
    let (agents, run) = parked(TurnErrorClass::PausedRateLimit, None);
    let (marker, _) = marker_plus(&agents, 0);
    assert_eq!(
        failed(action(
            &run,
            &agents,
            marker + FAIL_WINDOW,
            Continuation::Off,
            LimitReading::Refilled
        )),
        Some(LimitFailure {
            cause: LimitFailureCause::Refilled,
            refill_at: None,
            label: None,
        }),
        "a marker without a label stores none"
    );
}

#[test]
fn the_reading_is_taken_for_the_park_start() {
    let (agents, mut run) = parked(TurnErrorClass::PausedRateLimit, None);
    let (marker, now) = marker_plus(&agents, 30);
    run.started_at = marker - HOUR;
    let mut asked = None;
    park_action(
        &run,
        &agents,
        now,
        6 * HOUR,
        Continuation::On,
        |_, since| {
            asked = Some(since);
            LimitReading::Unknown
        },
    );
    assert_eq!(asked, Some(marker));
}

#[test]
fn a_park_that_turns_to_hold_inside_the_window_holds_and_one_that_acts_is_left() {
    let (agents, mut run) = parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
    run.started_at = agents[1].last_activity - hour();
    let (marker, now) = marker_plus(&agents, 30);
    assert!(action(&run, &agents, now, Continuation::On, LimitReading::Unknown).is_none());
    let later = now + Duration::from_secs(240);
    let reset = later + 2 * HOUR;
    let Some(ParkAction::Hold { child, hold }) = action(
        &run,
        &agents,
        later,
        Continuation::On,
        LimitReading::Refill(reset),
    ) else {
        panic!("a refill read inside the window holds from that pass");
    };
    assert_eq!(child.agent_id.as_str(), "child");
    assert_eq!(
        hold,
        LimitHold {
            activity: agents[1].last_activity,
            since: marker,
            until: reset,
        }
    );

    let mut acted = agents.clone();
    acted[1].last_activity = marker + Duration::from_secs(60);
    for continuation in [Continuation::Off, Continuation::On, Continuation::Spent] {
        assert!(
            action(
                &run,
                &acted,
                marker + FAIL_WINDOW,
                continuation,
                LimitReading::Unknown
            )
            .is_none(),
            "a child that acted inside the window is neither failed nor held"
        );
    }
}

#[test]
fn the_hold_starts_no_earlier_than_the_answer_it_pauses() {
    let (agents, mut run) = parked(TurnErrorClass::PausedRateLimit, None);
    let (marker, now) = marker_plus(&agents, 30);
    run.started_at = marker + Duration::from_secs(10);
    let Some(ParkAction::Hold { hold, .. }) = action(
        &run,
        &agents,
        now,
        Continuation::On,
        LimitReading::Refill(now + HOUR),
    ) else {
        panic!("held");
    };
    assert_eq!(hold.since, run.started_at);
}

#[test]
fn an_overload_pause_is_never_classified() {
    let (agents, run) = parked(
        TurnErrorClass::PausedOverloaded,
        Some("API Error: Overloaded"),
    );
    let (_, now) = marker_plus(&agents, 30);
    let (_, settled) = marker_plus(&agents, FAIL_WINDOW.as_secs() + 60);
    for at in [now, settled] {
        for reading in [
            LimitReading::Refill(at + HOUR),
            LimitReading::Refill(at + 30 * HOUR),
            LimitReading::Refilled,
            LimitReading::Unknown,
        ] {
            assert!(action(&run, &agents, at, Continuation::Off, reading).is_none());
        }
    }
}

#[test]
fn a_stored_hold_is_never_reclassified_and_fails_only_with_nothing_to_continue_it() {
    let (agents, mut run) = parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
    let (marker, now) = marker_plus(&agents, 30);
    let label = Some("Usage limit reached".to_owned());
    for until in [marker + Duration::from_secs(120), now + HOUR] {
        let hold = LimitHold {
            activity: agents[1].last_activity,
            since: marker,
            until,
        };
        run.limit_hold = Some(hold);
        run.park_noticed_activity = Some(hold.activity);
        let due = until.max(marker + FAIL_WINDOW);
        for continuation in [Continuation::Off, Continuation::Spent] {
            assert!(
                action(
                    &run,
                    &agents,
                    due - Duration::from_secs(1),
                    continuation,
                    LimitReading::Unknown
                )
                .is_none(),
                "before the later of the refill and the window"
            );
        }
        let Some(ParkAction::Fail {
            activity, failure, ..
        }) = action(&run, &agents, due, Continuation::Off, LimitReading::Unknown)
        else {
            panic!("off fails at {due}");
        };
        assert_eq!(activity, hold.activity);
        assert_eq!(
            failure,
            LimitFailure {
                cause: LimitFailureCause::RefillKnown,
                refill_at: Some(until),
                label: label.clone(),
            }
        );
        // A continue whose limit reply repeated the marker left this hold in place.
        assert_eq!(
            failed(action(
                &run,
                &agents,
                due,
                Continuation::Spent,
                LimitReading::Refilled
            )),
            Some(LimitFailure {
                cause: LimitFailureCause::ContinueFailed,
                refill_at: None,
                label: label.clone(),
            })
        );
        assert!(
            action(&run, &agents, due, Continuation::On, LimitReading::Unknown).is_none(),
            "auto-continue with retries left keeps the run open"
        );
    }

    let mut moved = agents.clone();
    moved[1].last_activity += Duration::from_secs(5);
    let due = run.limit_hold.unwrap().until;
    assert!(
        action(&run, &moved, due, Continuation::Off, LimitReading::Unknown).is_none(),
        "the child left that park"
    );
    let mut settled = run.clone();
    settled.status = RunStatus::Failed;
    assert!(
        action(
            &settled,
            &agents,
            due,
            Continuation::Off,
            LimitReading::Unknown
        )
        .is_none()
    );

    // A hold whose claim was released is claimed again as stored, whatever the reading.
    let hold = run.limit_hold.unwrap();
    run.park_noticed_activity = None;
    let Some(ParkAction::Hold { hold: again, .. }) = action(
        &run,
        &agents,
        now,
        Continuation::On,
        LimitReading::Refill(now + 30 * HOUR),
    ) else {
        panic!("a stored hold is claimed again");
    };
    assert_eq!(again, hold);
}

#[test]
fn the_detector_asks_for_one_helper_per_hold_or_failure_and_none_in_the_window() {
    let (agents, parked_run) = parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
    let (_, now) = marker_plus(&agents, 30);
    let (_, mark) = marker_plus(&agents, FAIL_WINDOW.as_secs());
    let mut told = parked_run.clone();
    told.run_id = RunId::new();
    told.park_noticed_activity = Some(agents[1].last_activity);
    let mut settled = parked_run.clone();
    settled.run_id = RunId::new();
    settled.status = RunStatus::Completed;
    let workspace_id = parked_run.workspace_id.clone();

    let ask = |now: Timestamp, reading: LimitReading, continuation: Continuation| {
        let mut asked = Vec::new();
        spawn_park_helpers_with(
            &[told.clone(), parked_run.clone(), settled.clone()],
            &agents,
            &workspace_id,
            now,
            2 * HOUR,
            |_| continuation,
            |_, _| reading,
            |request| asked.push(request.clone()),
        );
        asked
    };
    let expected = |continuation| {
        vec![ParkNoticeRequest {
            workspace_id: workspace_id.clone(),
            run_id: parked_run.run_id.clone(),
            horizon_secs: 2 * 3600,
            continuation,
        }]
    };
    assert_eq!(
        ask(now, LimitReading::Refill(now + HOUR), Continuation::On),
        expected(Continuation::On)
    );
    assert_eq!(
        ask(now, LimitReading::Refill(now + 3 * HOUR), Continuation::Off),
        expected(Continuation::Off),
        "a failure past the hold limit is the helper's at once"
    );
    for continuation in [Continuation::Off, Continuation::On, Continuation::Spent] {
        assert!(
            ask(now, LimitReading::Unknown, continuation).is_empty(),
            "nothing is spawned inside the window"
        );
    }
    assert!(ask(now, LimitReading::Refill(now + HOUR), Continuation::Off).is_empty());
    assert_eq!(
        ask(mark, LimitReading::Unknown, Continuation::Off),
        expected(Continuation::Off),
        "a failure after the window is the helper's"
    );
}

#[test]
fn quota_reads_as_refilled_only_from_a_usage_read_newer_than_the_park() {
    let since: Timestamp = "2026-01-01T12:00:00Z".parse().unwrap();
    let now = since + FAIL_WINDOW;
    let window = |used: u8, resets_at: Timestamp| crate::agents::RateLimitWindow {
        used_percentage: Some(used),
        duration_mins: Some(300),
        resets_at: Some(resets_at),
        ..Default::default()
    };
    let spent = ProviderCapacity::from_windows(vec![window(100, now + HOUR)]);
    let available = ProviderCapacity::from_windows(vec![window(5, now + HOUR)]);
    let after = Some(since + Duration::from_secs(1));
    assert_eq!(
        read_limit(Some(&spent), None, None, now, since),
        LimitReading::Refill(now + HOUR),
        "a refill ahead reads as it stands"
    );
    assert_eq!(
        read_limit(Some(&available), after, None, now, since),
        LimitReading::Refilled
    );
    for read_at in [None, Some(since), Some(since - Duration::from_secs(1))] {
        assert_eq!(
            read_limit(Some(&available), read_at, None, now, since),
            LimitReading::Unknown,
            "a read at {read_at:?} predates the park"
        );
    }
    assert_eq!(
        read_limit(None, after, None, now, since),
        LimitReading::Unknown
    );
    assert_eq!(
        read_limit(
            Some(&ProviderCapacity::from_windows(Vec::new())),
            after,
            None,
            now,
            since
        ),
        LimitReading::Unknown,
        "no window is no reading"
    );
}

#[test]
fn the_failure_tail_names_the_label_and_the_refill_without_a_duration_or_setting() {
    let refill: Timestamp = "2026-11-11T20:00:00Z".parse().unwrap();
    let at = format_local_timestamp(refill);
    let label = Some("Usage limit reached".to_owned());
    let lead = "stopped on a provider limit: \"Usage limit reached\"";
    for (cause, refill_at, label, expected) in [
        (
            LimitFailureCause::BeyondHoldLimit,
            Some(refill),
            label.clone(),
            format!("{lead}; it refills at {at}"),
        ),
        (
            LimitFailureCause::RefillKnown,
            Some(refill),
            None,
            format!("stopped on a provider limit; it refills at {at}"),
        ),
        (
            LimitFailureCause::ContinueFailed,
            None,
            label.clone(),
            format!("{lead}; continuing it after the refill failed"),
        ),
        (
            LimitFailureCause::Refilled,
            None,
            label.clone(),
            lead.to_owned(),
        ),
        (
            LimitFailureCause::RefillUnknown,
            None,
            None,
            "stopped on a provider limit".to_owned(),
        ),
    ] {
        let failure = LimitFailure {
            cause,
            refill_at,
            label,
        };
        assert_eq!(failure_tail(&failure), expected, "{cause:?}");
    }
}
