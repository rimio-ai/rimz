use super::*;
use jiff::civil::date;
use std::path::PathBuf;

const DURATION_SMHD: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];
const DURATION_SMH: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600)];

#[test]
fn duration_units_parse_and_reject_by_allowed_set() {
    for (raw, allowed, expected) in [
        ("30s", DURATION_SMH, std::time::Duration::from_secs(30)),
        ("5m", DURATION_SMH, std::time::Duration::from_secs(300)),
        ("1h", DURATION_SMH, std::time::Duration::from_secs(3600)),
        (
            "7d",
            DURATION_SMHD,
            std::time::Duration::from_secs(7 * 86_400),
        ),
    ] {
        assert_eq!(
            parse_duration_units(raw, allowed).unwrap(),
            expected,
            "{raw}"
        );
    }

    for (raw, allowed) in [
        ("30d", DURATION_SMH),
        ("30y", DURATION_SMHD),
        ("30", DURATION_SMH),
        ("", DURATION_SMH),
    ] {
        assert!(parse_duration_units(raw, allowed).is_err(), "{raw}");
    }
}

fn entry(at: Option<&str>, every: Option<&str>, cron: Option<&str>) -> TaskEntry {
    TaskEntry {
        agent: Some("claude".to_owned()),
        wake: None,
        prompt: Some("do it".to_owned()),
        prompt_file: None,
        check: None,
        verify: None,
        max_attempts: None,
        max_strikes: None,
        on: None,
        root: PathBuf::from("/home/me/app"),
        worktree: None,
        mode: None,
        effort: None,
        system_prompt_file: None,
        budget: None,
        budget_per_day: None,
        surplus: None,
        surplus_after: None,
        timeout: None,
        at: at.map(ToOwned::to_owned),
        every: every.map(ToOwned::to_owned),
        cron: cron.map(ToOwned::to_owned),
        deadline: None,
    }
}

#[test]
fn task_action_rejects_invalid_combinations() {
    let target = TaskTarget {
        kind: "claude".to_owned(),
        session: "sess".to_owned(),
        handle: "@claude".to_owned(),
    };
    let error = |entry: &TaskEntry| TaskAction::from_entry("task", entry).unwrap_err();
    assert!(matches!(
        error(&TaskEntry {
            agent: Some("claude".to_owned()),
            wake: Some(target.clone()),
            ..TaskEntry::default()
        }),
        TaskActionErr::ConflictingActions { .. }
    ));
    assert!(matches!(
        error(&TaskEntry {
            verify: Some("true".to_owned()),
            check: Some("true".to_owned()),
            ..TaskEntry::default()
        }),
        TaskActionErr::VerifyWithoutAgent { .. }
    ));
    assert!(matches!(
        error(&TaskEntry {
            agent: Some("claude".to_owned()),
            max_attempts: Some(2),
            ..TaskEntry::default()
        }),
        TaskActionErr::AttemptsWithoutVerify { .. }
    ));
    assert!(matches!(
        error(&TaskEntry {
            agent: Some("claude".to_owned()),
            verify: Some("true".to_owned()),
            max_attempts: Some(0),
            ..TaskEntry::default()
        }),
        TaskActionErr::ZeroAttempts { .. }
    ));
    assert!(matches!(
        error(&TaskEntry::default()),
        TaskActionErr::MissingAction { .. }
    ));
    assert!(matches!(
        TaskAction::from_entry(
            "task",
            &TaskEntry {
                wake: Some(target),
                ..TaskEntry::default()
            }
        ),
        Ok(TaskAction::Deliver(_))
    ));
}

#[test]
fn task_action_kind_predicates_cover_each_shape() {
    let target = TaskTarget {
        kind: "claude".to_owned(),
        session: "sess".to_owned(),
        handle: "@claude".to_owned(),
    };
    let cases = [
        (
            TaskEntry {
                agent: Some("claude".to_owned()),
                ..TaskEntry::default()
            },
            TaskActionKind::Spawn,
            (true, true, false),
        ),
        (
            TaskEntry {
                wake: Some(target),
                ..TaskEntry::default()
            },
            TaskActionKind::Deliver,
            (true, false, false),
        ),
        (
            TaskEntry {
                check: Some("true".to_owned()),
                ..TaskEntry::default()
            },
            TaskActionKind::CheckOnly,
            (false, false, true),
        ),
    ];

    for (entry, expected, predicates) in cases {
        let kind = TaskAction::from_entry("task", &entry).unwrap().kind();
        assert_eq!(kind, expected);
        assert_eq!(
            (kind.has_effect(), kind.is_spawn(), kind.is_check_only()),
            predicates
        );
    }
}

#[test]
fn task_shape_keeps_action_reset_identity_and_timing_independent() {
    let mut conflicting = reset_entry(Some("claude-ping"));
    conflicting.cron = Some("0 7 * * *".to_owned());
    let shape = TaskShape::compile("primer", &conflicting);
    assert_eq!(
        shape.action(),
        Ok(&TaskAction::Spawn("claude-ping".to_owned()))
    );
    assert!(matches!(
        shape.schedule(),
        Err(ScheduleErr::TimeConflict { .. })
    ));
    assert_eq!(
        shape.reset_ping_kind().map(AgentKind::as_str),
        Some("claude")
    );

    for (every, agent, expected_kind) in [
        ("reset", "claude-ping", Some("claude")),
        ("Reset", "claude-ping", None),
        (" reset ", "claude-ping", None),
        ("reset", "amp-ping", None),
    ] {
        let mut entry = reset_entry(Some(agent));
        entry.every = Some(every.to_owned());
        assert_eq!(
            TaskShape::compile("primer", &entry)
                .reset_ping_kind()
                .map(AgentKind::as_str),
            expected_kind,
            "{every} {agent}"
        );
    }
}

#[test]
fn surplus_gate_values_parse_and_reject_unsafe_inputs() {
    assert_eq!(parse_surplus("1.5x"), Ok(1.5));
    assert_eq!(parse_surplus(" 2X "), Ok(2.0));
    assert_eq!(
        parse_surplus_after("3d"),
        Ok(Duration::from_secs(3 * 86_400))
    );
    for raw in ["", "0", "-1x", "NaN", "inf", "many"] {
        assert!(parse_surplus(raw).is_err(), "{raw}");
    }
    for raw in ["3", "2w", "1.5d"] {
        assert!(parse_surplus_after(raw).is_err(), "{raw}");
    }
}

#[test]
fn weekdays_describe_in_mon_to_sun_order() {
    let parsed =
        parse_schedule("morning", &entry(Some("07:30"), Some("weekdays"), None)).expect("parse");
    assert_eq!(parsed.describe(), "every Mon,Tue,Wed,Thu,Fri at 07:30");
}

#[test]
fn bare_at_is_a_one_shot() {
    let parsed = parse_schedule("m", &entry(Some("07:00"), None, None)).expect("parse");
    assert!(parsed.once);
    assert_eq!(parsed.describe(), "once at 07:00");
}

#[test]
fn daily_is_the_empty_day_mask() {
    let from_day = parse_schedule("m", &entry(Some("07:00"), Some("day"), None))
        .expect("parse")
        .schedule;
    let from_daily = parse_schedule("m", &entry(Some("07:00"), Some("daily"), None))
        .expect("parse")
        .schedule;
    assert_eq!(from_day, from_daily);
    assert_eq!(from_day.describe(), "every day at 07:00");
}

#[test]
fn day_lists_ranges_and_weekends_parse() {
    let list = parse_schedule("m", &entry(Some("06:05"), Some("mon,wed,fri"), None))
        .expect("parse")
        .schedule;
    assert_eq!(
        list,
        Schedule::Calendar(CalendarSpec {
            minute: 5,
            hour: 6,
            weekdays: vec![Weekday::Mon, Weekday::Wed, Weekday::Fri],
        })
    );
    let range = parse_schedule("m", &entry(Some("06:00"), Some("mon-fri"), None))
        .expect("parse")
        .schedule;
    assert_eq!(range.describe(), "every Mon,Tue,Wed,Thu,Fri at 06:00");
    let weekends = parse_schedule("m", &entry(Some("09:00"), Some("weekends"), None))
        .expect("parse")
        .schedule;
    assert_eq!(weekends.describe(), "every Sat,Sun at 09:00");
}

#[test]
fn raw_cron_passes_through() {
    let parsed = parse_schedule("m", &entry(None, None, Some("0 7 * * 1-5"))).expect("parse");
    let schedule = parsed.schedule;
    assert!(!parsed.once);
    assert_eq!(schedule, Schedule::RawCron("0 7 * * 1-5".to_owned()));
    assert_eq!(schedule.describe(), "cron `0 7 * * 1-5`");
}

#[test]
fn intervals_describe_exact_duration() {
    let schedule = parse_schedule("m", &entry(None, Some("7m"), None))
        .expect("parse")
        .schedule;
    assert_eq!(schedule.describe(), "every 7m");
}

#[test]
fn seconds_interval_rounds_up_to_one_minute() {
    let schedule = parse_schedule("m", &entry(None, Some("1s"), None))
        .expect("parse")
        .schedule;
    assert_eq!(schedule.describe(), "every 1m");
}

#[test]
fn conflicting_and_missing_times_error() {
    assert_eq!(
        parse_schedule("m", &entry(Some("07:00"), None, Some("0 7 * * *"))),
        Err(ScheduleErr::TimeConflict {
            name: "m".to_owned()
        })
    );
    assert_eq!(
        parse_schedule("m", &entry(None, None, None)),
        Err(ScheduleErr::NoTime {
            name: "m".to_owned()
        })
    );
    assert_eq!(
        parse_schedule("m", &entry(None, Some("weekdays"), None)),
        Err(ScheduleErr::EveryNeedsAt {
            name: "m".to_owned()
        })
    );
    assert_eq!(
        parse_schedule("m", &entry(Some("07:00"), Some("5m"), None)),
        Err(ScheduleErr::TimeConflict {
            name: "m".to_owned()
        })
    );
}

#[test]
fn bad_time_day_and_cron_are_rejected() {
    assert!(matches!(
        parse_schedule("m", &entry(Some("7am"), None, None)),
        Err(ScheduleErr::BadTime { .. })
    ));
    assert!(matches!(
        parse_schedule("m", &entry(Some("24:00"), None, None)),
        Err(ScheduleErr::BadTime { .. })
    ));
    assert!(matches!(
        parse_schedule("m", &entry(Some("07:00"), Some("funday"), None)),
        Err(ScheduleErr::BadEvery { .. })
    ));
    assert!(matches!(
        parse_schedule("m", &entry(None, None, Some("0 7 * *"))),
        Err(ScheduleErr::BadCron { .. })
    ));
    assert!(matches!(
        parse_schedule("m", &entry(None, Some("0m"), None)),
        Err(ScheduleErr::BadEvery { .. })
    ));
    assert!(matches!(
        parse_schedule("m", &entry(None, Some("later"), None)),
        Err(ScheduleErr::BadEvery { .. })
    ));
    assert!(matches!(
        parse_schedule("m", &entry(None, Some("Reset"), None)),
        Err(ScheduleErr::BadEvery { .. })
    ));
    assert!(matches!(
        parse_schedule("m", &entry(None, Some(" reset "), None)),
        Err(ScheduleErr::BadEvery { .. })
    ));
}

#[test]
fn names_are_validated() {
    validate_name("morning").expect("ok");
    validate_name("morning-claude_1").expect("ok");
    assert!(validate_name("").is_err());
    assert!(validate_name("bad name").is_err());
    assert!(validate_name("bad/name").is_err());
}

fn zdt(year: i16, month: i8, day: i8, hour: i8, minute: i8, second: i8) -> Zoned {
    date(year, month, day)
        .at(hour, minute, second, 0)
        .in_tz("UTC")
        .expect("zoned test time")
}

fn seconds_before(ts: Timestamp, seconds: i64) -> Timestamp {
    Timestamp::from_second(ts.as_second() - seconds).expect("shifted timestamp")
}

fn evaluate_timing(
    name: &str,
    entry: &TaskEntry,
    blocked: Option<crate::trust::TrustState>,
    last_fire: Option<Timestamp>,
    pause: Option<&pauses::PauseEntry>,
    now: &Zoned,
    reset_signal: ResetSignal,
) -> TaskTiming {
    TaskTiming::evaluate(
        &parse_schedule(name, entry),
        blocked,
        last_fire,
        pause,
        now,
        reset_signal,
    )
}

#[test]
fn task_timing_classifies_runtime_schedule_edges() {
    let now = zdt(2026, 6, 24, 8, 10, 0);
    let interval = entry(None, Some("15m"), None);
    assert!(matches!(
        evaluate_timing(
            "task",
            &interval,
            None,
            Some(seconds_before(now.timestamp(), 10 * 60)),
            None,
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::Upcoming(_)
    ));
    assert!(matches!(
        evaluate_timing(
            "task",
            &interval,
            None,
            Some(seconds_before(now.timestamp(), 20 * 60)),
            None,
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::Due(_)
    ));
    assert_eq!(
        evaluate_timing(
            "task",
            &interval,
            None,
            None,
            None,
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::Unarmed
    );

    let reset = reset_entry(Some("claude-ping"));
    assert_eq!(
        evaluate_timing(
            "task",
            &reset,
            None,
            Some(seconds_before(now.timestamp(), 60)),
            None,
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::NoOccurrence
    );

    let invalid = entry(None, None, None);
    assert_eq!(
        evaluate_timing(
            "task",
            &invalid,
            None,
            Some(now.timestamp()),
            None,
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::Invalid
    );
}

#[test]
fn task_timing_precedence_is_blocked_then_pause_then_schedule() {
    let now = zdt(2026, 6, 24, 8, 10, 0);
    let invalid = entry(None, None, None);
    let manual = pauses::PauseEntry {
        until: None,
        strikes: Some(3),
    };
    assert_eq!(
        evaluate_timing(
            "task",
            &invalid,
            Some(crate::trust::TrustState::Stale),
            None,
            Some(&manual),
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::Blocked(crate::trust::TrustState::Stale)
    );
    assert_eq!(
        evaluate_timing(
            "task",
            &invalid,
            None,
            None,
            Some(&manual),
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::Paused(manual)
    );

    let timed = pauses::PauseEntry {
        until: now
            .timestamp()
            .checked_add(SignedDuration::from_secs(5 * 60))
            .ok(),
        strikes: None,
    };
    assert_eq!(
        evaluate_timing(
            "task",
            &invalid,
            None,
            None,
            Some(&timed),
            &now,
            ResetSignal::Unknown,
        )
        .state(),
        TaskTimingState::Paused(timed)
    );
}

#[test]
fn task_timing_uses_expired_pause_as_last_fire_edge() {
    let now = zdt(2026, 6, 24, 8, 10, 0);
    let interval = entry(None, Some("15m"), None);
    let pause = pauses::PauseEntry {
        until: Some(seconds_before(now.timestamp(), 5 * 60)),
        strikes: None,
    };
    let timing = evaluate_timing(
        "task",
        &interval,
        None,
        Some(seconds_before(now.timestamp(), 20 * 60)),
        Some(&pause),
        &now,
        ResetSignal::Unknown,
    );
    assert_eq!(
        timing.state(),
        TaskTimingState::Upcoming(
            now.timestamp()
                .checked_add(SignedDuration::from_secs(10 * 60))
                .expect("next interval")
        )
    );
}

#[test]
fn task_timing_due_matches_schedule_rules_at_occurrence_edges() {
    let now = zdt(2026, 6, 24, 8, 10, 0);
    let reset = seconds_before(now.timestamp(), 60);
    for (name, entry, last_fire, reset_signal) in [
        (
            "calendar",
            entry(Some("08:10"), None, None),
            seconds_before(now.timestamp(), 60),
            ResetSignal::Unknown,
        ),
        (
            "interval",
            entry(None, Some("15m"), None),
            seconds_before(now.timestamp(), 15 * 60),
            ResetSignal::Unknown,
        ),
        (
            "cron",
            entry(None, None, Some("10 8 * * *")),
            seconds_before(now.timestamp(), 60),
            ResetSignal::Unknown,
        ),
        (
            "reset",
            reset_entry(Some("claude-ping")),
            seconds_before(now.timestamp(), 60),
            ResetSignal::At(reset),
        ),
    ] {
        let parsed = parse_schedule(name, &entry).expect("parsed schedule");
        assert!(parsed.schedule.due(last_fire, &now, reset_signal), "{name}");
        assert!(matches!(
            evaluate_timing(
                name,
                &entry,
                None,
                Some(last_fire),
                None,
                &now,
                reset_signal,
            )
            .state(),
            TaskTimingState::Due(_)
        ));
    }
}

fn reset_entry(agent: Option<&str>) -> TaskEntry {
    let mut entry = entry(None, Some("reset"), None);
    entry.agent = agent.map(ToOwned::to_owned);
    entry
}

#[test]
fn every_reset_parse_requires_ping_agent() {
    let parsed = parse_schedule("w", &reset_entry(Some("claude-ping"))).expect("reset ping");
    assert_eq!(parsed.schedule, Schedule::WindowReset);
    assert_eq!(parsed.describe(), "every window reset");

    let mut conflict = reset_entry(Some("claude-ping"));
    conflict.at = Some("07:00".to_owned());
    assert_eq!(
        parse_schedule("w", &conflict),
        Err(ScheduleErr::TimeConflict {
            name: "w".to_owned()
        })
    );

    assert_eq!(
        parse_schedule("w", &reset_entry(Some("claude"))),
        Err(ScheduleErr::ResetNeedsPing {
            name: "w".to_owned()
        })
    );
    assert_eq!(
        parse_schedule("w", &reset_entry(None)),
        Err(ScheduleErr::ResetNeedsPing {
            name: "w".to_owned()
        })
    );

    let mut check_only = reset_entry(None);
    check_only.check = Some("true".to_owned());
    assert_eq!(
        parse_schedule("w", &check_only),
        Err(ScheduleErr::ResetNeedsPing {
            name: "w".to_owned()
        })
    );

    let mut wake = reset_entry(None);
    wake.wake = Some(crate::config::TaskTarget {
        kind: "claude".to_owned(),
        session: "sess".to_owned(),
        handle: "@claude".to_owned(),
    });
    assert_eq!(
        parse_schedule("w", &wake),
        Err(ScheduleErr::ResetNeedsPing {
            name: "w".to_owned()
        })
    );
}

#[test]
fn interval_due_at_exact_boundary_only() {
    let schedule = Schedule::Interval(IntervalSpec::new(15));
    let now = zdt(2026, 6, 24, 8, 15, 0);
    assert!(!schedule.due(
        seconds_before(now.timestamp(), 899),
        &now,
        ResetSignal::Unknown
    ));
    assert!(schedule.due(
        seconds_before(now.timestamp(), 900),
        &now,
        ResetSignal::Unknown
    ));
}

#[test]
fn calendar_fires_once_per_matching_day() {
    let schedule = parse_schedule("m", &entry(Some("07:30"), Some("wed"), None))
        .expect("parse")
        .schedule;
    let now = zdt(2026, 6, 24, 7, 30, 0);
    let occurrence = now.timestamp();
    assert!(schedule.due(seconds_before(occurrence, 60), &now, ResetSignal::Unknown));
    assert!(!schedule.due(occurrence, &now, ResetSignal::Unknown));
}

#[test]
fn window_reset_due_uses_reset_plus_margin_once() {
    let schedule = Schedule::WindowReset;
    let reset = zdt(2026, 6, 24, 8, 0, 0).timestamp();
    let before = zdt(2026, 6, 24, 8, 0, 59);
    let at_margin = zdt(2026, 6, 24, 8, 1, 0);
    let occurrence = at_margin.timestamp();

    assert!(!schedule.due(seconds_before(reset, 60), &before, ResetSignal::At(reset)));
    assert!(schedule.due(
        seconds_before(occurrence, 60),
        &at_margin,
        ResetSignal::At(reset)
    ));
    assert!(!schedule.due(occurrence, &at_margin, ResetSignal::At(reset)));
    assert!(!schedule.due(
        seconds_before(occurrence, 60),
        &at_margin,
        ResetSignal::Unknown
    ));
}

#[test]
fn confirmed_down_window_reset_retries_at_hourly_edge() {
    let schedule = Schedule::WindowReset;
    let now = zdt(2026, 6, 24, 8, 0, 0);
    let one_hour_ago = seconds_before(now.timestamp(), RESET_RETRY_INTERVAL.as_secs());

    assert!(
        !schedule.due(
            one_hour_ago
                .checked_add(SignedDuration::from_secs(1))
                .expect("inside retry interval"),
            &now,
            ResetSignal::ConfirmedDown
        )
    );
    assert!(schedule.due(one_hour_ago, &now, ResetSignal::ConfirmedDown));
    assert_eq!(
        schedule.next_after(one_hour_ago, &now, ResetSignal::ConfirmedDown),
        Some(now.timestamp())
    );
    assert_eq!(
        schedule.next_after(one_hour_ago, &now, ResetSignal::Unknown),
        None
    );
}

#[test]
fn calendar_waits_for_matching_weekday_and_time() {
    let weekday_schedule = parse_schedule("m", &entry(Some("07:30"), Some("mon"), None))
        .expect("parse")
        .schedule;
    let wednesday = zdt(2026, 6, 24, 7, 30, 0);
    assert!(!weekday_schedule.due(
        seconds_before(wednesday.timestamp(), 86_400),
        &wednesday,
        ResetSignal::Unknown
    ));

    let time_schedule = parse_schedule("m", &entry(Some("07:30"), None, None))
        .expect("parse")
        .schedule;
    let before_time = zdt(2026, 6, 24, 7, 29, 59);
    assert!(!time_schedule.due(
        seconds_before(before_time.timestamp(), 86_400),
        &before_time,
        ResetSignal::Unknown
    ));
}

#[test]
fn cron_interval_matches_and_suppresses_same_minute() {
    let schedule = Schedule::RawCron("*/15 * * * *".to_owned());
    let now = zdt(2026, 6, 24, 8, 30, 12);
    assert!(schedule.due(
        seconds_before(now.timestamp(), 60),
        &now,
        ResetSignal::Unknown
    ));
    assert!(!schedule.due(
        seconds_before(now.timestamp(), 1),
        &now,
        ResetSignal::Unknown
    ));

    let off_minute = zdt(2026, 6, 24, 8, 31, 0);
    assert!(!schedule.due(
        seconds_before(off_minute.timestamp(), 60),
        &off_minute,
        ResetSignal::Unknown
    ));
}

#[test]
fn cron_weekday_gates() {
    let schedule = Schedule::RawCron("0 7 * * 1-5".to_owned());
    let wednesday = zdt(2026, 6, 24, 7, 0, 0);
    assert!(schedule.due(
        seconds_before(wednesday.timestamp(), 60),
        &wednesday,
        ResetSignal::Unknown
    ));

    let saturday = zdt(2026, 6, 27, 7, 0, 0);
    assert!(!schedule.due(
        seconds_before(saturday.timestamp(), 60),
        &saturday,
        ResetSignal::Unknown
    ));
}

#[test]
fn interval_next_after_uses_last_fire_edge() {
    let schedule = Schedule::Interval(IntervalSpec::new(15));
    let now = zdt(2026, 6, 24, 8, 10, 0);
    let last_fire = seconds_before(now.timestamp(), 60);
    assert_eq!(
        schedule.next_after(last_fire, &now, ResetSignal::Unknown),
        Some(Timestamp::from_second(last_fire.as_second() + 900).expect("timestamp"))
    );
}

#[test]
fn window_reset_next_after_reports_unconsumed_occurrence() {
    let schedule = Schedule::WindowReset;
    let now = zdt(2026, 6, 24, 8, 0, 0);
    let reset = now.timestamp();
    let occurrence = reset
        .checked_add(RESET_PING_MARGIN)
        .expect("reset occurrence");

    assert_eq!(
        schedule.next_after(seconds_before(occurrence, 1), &now, ResetSignal::At(reset)),
        Some(occurrence)
    );
    assert_eq!(
        schedule.next_after(occurrence, &now, ResetSignal::At(reset)),
        None
    );
    assert_eq!(
        schedule.next_after(seconds_before(occurrence, 1), &now, ResetSignal::Unknown),
        None
    );
}

#[test]
fn calendar_next_after_crosses_week_boundary() {
    let schedule = parse_schedule("m", &entry(Some("07:30"), Some("mon"), None))
        .expect("parse")
        .schedule;
    let now = zdt(2026, 6, 24, 8, 0, 0);
    let last_fire = zdt(2026, 6, 22, 7, 30, 0).timestamp();
    assert_eq!(
        schedule.next_after(last_fire, &now, ResetSignal::Unknown),
        Some(zdt(2026, 6, 29, 7, 30, 0).timestamp())
    );
}

#[test]
fn calendar_next_after_reports_due_today() {
    let schedule = parse_schedule("m", &entry(Some("07:30"), None, None))
        .expect("parse")
        .schedule;
    let now = zdt(2026, 6, 24, 8, 0, 0);
    let last_fire = zdt(2026, 6, 23, 7, 30, 0).timestamp();
    assert_eq!(
        schedule.next_after(last_fire, &now, ResetSignal::Unknown),
        Some(zdt(2026, 6, 24, 7, 30, 0).timestamp())
    );
}

#[test]
fn cron_next_after_walks_to_next_matching_minute() {
    let schedule = Schedule::RawCron("*/15 * * * *".to_owned());
    let now = zdt(2026, 6, 24, 8, 14, 12);
    assert_eq!(
        schedule.next_after(
            seconds_before(now.timestamp(), 60),
            &now,
            ResetSignal::Unknown
        ),
        Some(zdt(2026, 6, 24, 8, 15, 0).timestamp())
    );
}

#[test]
fn cron_next_after_reports_current_matching_minute_due_once() {
    let schedule = Schedule::RawCron("*/15 * * * *".to_owned());
    let now = zdt(2026, 6, 24, 8, 30, 12);
    assert_eq!(
        schedule.next_after(
            seconds_before(now.timestamp(), 60),
            &now,
            ResetSignal::Unknown
        ),
        Some(now.timestamp())
    );
}

#[test]
fn cron_next_after_returns_none_past_search_cap() {
    let schedule = Schedule::RawCron("0 0 1 1 *".to_owned());
    let now = zdt(2026, 1, 2, 0, 0, 0);
    assert_eq!(
        schedule.next_after(
            seconds_before(now.timestamp(), 60),
            &now,
            ResetSignal::Unknown
        ),
        None
    );
}
