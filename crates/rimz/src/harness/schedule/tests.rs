use super::*;
use jiff::civil::date;

use Weekday::{Fri, Mon, Wed};

const DURATION_SMHD: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];
const DURATION_SMH: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600)];

pub(super) fn zdt(year: i16, month: i8, day: i8, hour: i8, minute: i8, second: i8) -> Zoned {
    date(year, month, day)
        .at(hour, minute, second, 0)
        .in_tz("UTC")
        .expect("zoned test time")
}

pub(super) fn seconds_before(ts: Timestamp, seconds: i64) -> Timestamp {
    Timestamp::from_second(ts.as_second() - seconds).expect("shifted timestamp")
}

/// A last-fire stamp `seconds` before `now`.
fn before(now: &Zoned, seconds: i64) -> Timestamp {
    seconds_before(now.timestamp(), seconds)
}

/// An occurrence `seconds` after `now`.
fn after(now: &Zoned, seconds: i64) -> Timestamp {
    now.timestamp()
        .checked_add(SignedDuration::from_secs(seconds))
        .expect("shifted timestamp")
}

fn entry(at: Option<&str>, every: Option<&str>, cron: Option<&str>) -> TaskEntry {
    TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("do it".to_owned()),
        at: at.map(ToOwned::to_owned),
        every: every.map(ToOwned::to_owned),
        cron: cron.map(ToOwned::to_owned),
        ..TaskEntry::default()
    }
}

fn spawn_entry() -> TaskEntry {
    TaskEntry {
        agent: Some("claude".to_owned()),
        ..TaskEntry::default()
    }
}

fn wake_target() -> TaskTarget {
    TaskTarget {
        kind: "claude".to_owned(),
        session: "sess".to_owned(),
        handle: "@claude".to_owned(),
    }
}

fn schedule_of(entry: &TaskEntry) -> Schedule {
    parse_schedule("m", entry).expect("parse").schedule
}

#[test]
fn duration_units_parse_and_reject_by_allowed_set() {
    for (raw, allowed, expected) in [
        ("30s", DURATION_SMH, Duration::from_secs(30)),
        ("5m", DURATION_SMH, Duration::from_secs(300)),
        ("1h", DURATION_SMH, Duration::from_secs(3600)),
        ("7d", DURATION_SMHD, Duration::from_secs(7 * 86_400)),
    ] {
        assert_eq!(parse_duration_units(raw, allowed), Ok(expected), "{raw}");
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
fn names_are_validated() {
    validate_name("morning").expect("ok");
    validate_name("morning-claude_1").expect("ok");
    for name in ["", "bad name", "bad/name"] {
        assert!(validate_name(name).is_err(), "{name}");
    }
}

/// Each accepted action shape with its kind predicates, then every way the
/// action fields contradict each other and the message that names the fix.
#[test]
fn task_action_from_entry_maps_field_combinations() {
    // (entry, kind, (has_effect, is_spawn, is_check_only))
    for (entry, kind, predicates) in [
        (spawn_entry(), TaskActionKind::Spawn, (true, true, false)),
        (
            TaskEntry {
                wake: Some(wake_target()),
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
    ] {
        let action = TaskAction::from_entry("task", &entry).expect("valid action");
        assert_eq!(action.kind(), kind, "{entry:?}");
        assert_eq!(
            (kind.has_effect(), kind.is_spawn(), kind.is_check_only()),
            predicates,
            "{entry:?}"
        );
        assert_eq!(action.is_check_only(), predicates.2, "{entry:?}");
    }

    for (entry, message) in [
        (
            TaskEntry {
                wake: Some(wake_target()),
                ..spawn_entry()
            },
            "loop task `task` sets both `agent` and `wake`; keep exactly one",
        ),
        (
            TaskEntry {
                verify: Some("true".to_owned()),
                check: Some("true".to_owned()),
                ..TaskEntry::default()
            },
            "loop task `task` sets `verify` without `agent`; verification needs a supervised agent run",
        ),
        (
            TaskEntry {
                max_attempts: Some(2),
                ..spawn_entry()
            },
            "loop task `task` sets `max-attempts` without `verify`",
        ),
        (
            TaskEntry {
                verify: Some("true".to_owned()),
                max_attempts: Some(0),
                ..spawn_entry()
            },
            "loop task `task` sets `max-attempts` to 0; use at least 1",
        ),
        (
            TaskEntry::default(),
            "loop task `task` needs `agent`, `wake`, or `check`",
        ),
    ] {
        let err = TaskAction::from_entry("task", &entry).expect_err("invalid action");
        assert_eq!(err.to_string(), message);
    }
}

/// A row compiles into independent action and timing halves, so a malformed
/// schedule leaves the action observable.
#[test]
fn task_shape_compiles_action_and_timing_independently() {
    let shape = TaskShape::compile(
        "morning",
        &TaskEntry {
            cron: Some("0 7 * * *".to_owned()),
            every: Some("weekday".to_owned()),
            agent: Some("claude".to_owned()),
            ..TaskEntry::default()
        },
    );
    assert_eq!(shape.action(), Ok(&TaskAction::Spawn("claude".to_owned())));
    assert!(matches!(
        shape.schedule(),
        Err(ScheduleErr::TimeConflict { .. })
    ));
}

/// Every accepted `(at, every, cron)` shape and the line a listing renders.
/// `describe` is a total projection of the parsed schedule, so the structural
/// assertions below pin the shape it renders from.
#[test]
fn parse_schedule_accepts_every_timing_form() {
    let described = |at, every, cron| {
        parse_schedule("m", &entry(at, every, cron))
            .expect("parse")
            .describe()
    };
    for (at, every, cron, expected) in [
        (Some("07:00"), None, None, "once at 07:00"),
        (Some("07:00"), Some("day"), None, "every day at 07:00"),
        (Some("07:00"), Some("daily"), None, "every day at 07:00"),
        (
            Some("06:05"),
            Some("mon,wed,fri"),
            None,
            "every Mon,Wed,Fri at 06:05",
        ),
        (
            Some("09:00"),
            Some("weekends"),
            None,
            "every Sat,Sun at 09:00",
        ),
        (
            Some("07:30"),
            Some("weekdays"),
            None,
            "every Mon,Tue,Wed,Thu,Fri at 07:30",
        ),
        (
            Some("06:00"),
            Some("mon-fri"),
            None,
            "every Mon,Tue,Wed,Thu,Fri at 06:00",
        ),
        (None, Some("7m"), None, "every 7m"),
        // Sub-minute intervals round up rather than flooring to never firing.
        (None, Some("1s"), None, "every 1m"),
        (None, None, Some("0 7 * * 1-5"), "cron `0 7 * * 1-5`"),
    ] {
        let label = format!("{at:?} {every:?} {cron:?}");
        assert_eq!(described(at, every, cron), expected, "{label}");
    }

    assert_eq!(
        schedule_of(&entry(Some("06:05"), Some("mon,wed,fri"), None)),
        Schedule::Calendar(CalendarSpec {
            minute: 5,
            hour: 6,
            weekdays: vec![Mon, Wed, Fri],
        })
    );
    assert_eq!(
        schedule_of(&entry(None, Some("7m"), None)),
        Schedule::Interval(IntervalSpec::new(7))
    );
    assert_eq!(
        schedule_of(&entry(None, None, Some("0 7 * * 1-5"))),
        Schedule::RawCron("0 7 * * 1-5".to_owned())
    );
    // Only a bare `at` is a one-shot; a repeat cadence always outlives its fire.
    for (at, every, cron, once) in [
        (Some("07:00"), None, None, true),
        (Some("07:00"), Some("day"), None, false),
        (None, None, Some("0 7 * * 1-5"), false),
    ] {
        let parsed = parse_schedule("m", &entry(at, every, cron)).expect("parse");
        assert_eq!(parsed.once, once, "{at:?} {every:?} {cron:?}");
    }
}

/// Every `ScheduleErr` variant with a field combination that raises it.
#[test]
fn parse_schedule_rejects_invalid_timing_fields() {
    let name = || "m".to_owned();
    let conflict = || ScheduleErr::TimeConflict { name: name() };
    let bad_time = |value: &str| ScheduleErr::BadTime {
        name: name(),
        value: value.to_owned(),
    };
    let bad_every = |value: &str| ScheduleErr::BadEvery {
        name: name(),
        value: value.to_owned(),
    };
    let bad_cron = |value: &str| ScheduleErr::BadCron {
        name: name(),
        value: value.to_owned(),
    };

    for (at, every, cron, expected) in [
        (Some("07:00"), None, Some("0 7 * * *"), conflict()),
        (Some("07:00"), Some("5m"), None, conflict()),
        (None, None, None, ScheduleErr::NoTime { name: name() }),
        (
            None,
            Some("weekdays"),
            None,
            ScheduleErr::EveryNeedsAt { name: name() },
        ),
        (Some("7am"), None, None, bad_time("7am")),
        (Some("24:00"), None, None, bad_time("24:00")),
        (Some("07:00"), Some("funday"), None, bad_every("funday")),
        (None, Some("0m"), None, bad_every("0m")),
        (None, Some("later"), None, bad_every("later")),
        (None, Some("reset"), None, bad_every("reset")),
        (None, Some("Reset"), None, bad_every("Reset")),
        (None, Some(" reset "), None, bad_every(" reset ")),
        (None, None, Some("0 7 * *"), bad_cron("0 7 * *")),
    ] {
        let label = format!("{at:?} {every:?} {cron:?}");
        assert_eq!(
            parse_schedule("m", &entry(at, every, cron)),
            Err(expected),
            "{label}"
        );
    }
}

/// `due` on both sides of every occurrence edge, so a one-second slip in either
/// direction fails.
#[test]
fn schedule_due_at_occurrence_edges() {
    let interval = Schedule::Interval(IntervalSpec::new(15));
    let quarter_hour = Schedule::RawCron("*/15 * * * *".to_owned());
    let weekday_cron = Schedule::RawCron("0 7 * * 1-5".to_owned());
    let every_wed = schedule_of(&entry(Some("07:30"), Some("wed"), None));
    let every_mon = schedule_of(&entry(Some("07:30"), Some("mon"), None));
    let daily = schedule_of(&entry(Some("07:30"), None, None));

    let boundary = zdt(2026, 6, 24, 8, 15, 0);
    let wed_0730 = zdt(2026, 6, 24, 7, 30, 0);
    let wed_0700 = zdt(2026, 6, 24, 7, 0, 0);
    let wed_0729 = zdt(2026, 6, 24, 7, 29, 59);
    let sat_0700 = zdt(2026, 6, 27, 7, 0, 0);
    let quarter = zdt(2026, 6, 24, 8, 30, 12);
    let off_minute = zdt(2026, 6, 24, 8, 31, 0);
    // Was `schedule` due at `now`, given a fire `ago` seconds earlier?
    let due = |schedule: &Schedule, now: &Zoned, ago| schedule.due(before(now, ago), now);

    assert!(!due(&interval, &boundary, 899), "interval a second early");
    assert!(due(&interval, &boundary, 900), "interval at the boundary");
    assert!(due(&every_wed, &wed_0730, 60), "calendar at its occurrence");
    assert!(
        !due(&every_wed, &wed_0730, 0),
        "calendar already fired today"
    );
    assert!(
        !due(&every_mon, &wed_0730, 86_400),
        "calendar off its weekday"
    );
    assert!(!due(&daily, &wed_0729, 86_400), "calendar a second early");
    assert!(
        due(&quarter_hour, &quarter, 60),
        "cron on a matching minute"
    );
    assert!(
        !due(&quarter_hour, &quarter, 1),
        "cron already fired this minute"
    );
    assert!(!due(&quarter_hour, &off_minute, 60), "cron off its minute");
    assert!(due(&weekday_cron, &wed_0700, 60), "cron on a weekday");
    assert!(
        !due(&weekday_cron, &sat_0700, 60),
        "cron gated off the weekend"
    );
}

/// `next_after` for every schedule variant, including the bounded cron walk that
/// gives up rather than spinning.
#[test]
fn schedule_next_after_reports_the_following_occurrence() {
    let interval = Schedule::Interval(IntervalSpec::new(15));
    let quarter_hour = Schedule::RawCron("*/15 * * * *".to_owned());
    let yearly = Schedule::RawCron("0 0 1 1 *".to_owned());
    let every_mon = schedule_of(&entry(Some("07:30"), Some("mon"), None));
    let daily = schedule_of(&entry(Some("07:30"), None, None));

    let morning = zdt(2026, 6, 24, 8, 0, 0);
    let ten_past = zdt(2026, 6, 24, 8, 10, 0);
    let before_quarter = zdt(2026, 6, 24, 8, 14, 12);
    let on_quarter = zdt(2026, 6, 24, 8, 30, 12);
    let jan_second = zdt(2026, 1, 2, 0, 0, 0);
    // The next occurrence at `now`, given a fire `ago` seconds earlier.
    let next = |schedule: &Schedule, now: &Zoned, ago| schedule.next_after(before(now, ago), now);

    let interval_last = before(&ten_past, 60);
    assert_eq!(
        next(&interval, &ten_past, 60),
        Timestamp::from_second(interval_last.as_second() + 900).ok(),
        "interval counts from the last fire"
    );
    assert_eq!(
        next(&every_mon, &morning, 2 * 86_400 + 1_800),
        Some(zdt(2026, 6, 29, 7, 30, 0).timestamp()),
        "calendar crosses the week from Mon Jun 22"
    );
    assert_eq!(
        next(&daily, &morning, 86_400 + 1_800),
        Some(zdt(2026, 6, 24, 7, 30, 0).timestamp()),
        "calendar reports today's missed occurrence"
    );
    assert_eq!(
        next(&quarter_hour, &before_quarter, 60),
        Some(zdt(2026, 6, 24, 8, 15, 0).timestamp()),
        "cron walks to the next matching minute"
    );
    assert_eq!(
        next(&quarter_hour, &on_quarter, 60),
        Some(on_quarter.timestamp()),
        "cron reports its current matching minute once"
    );
    assert_eq!(
        next(&yearly, &jan_second, 60),
        None,
        "cron gives up past the search cap"
    );
}

/// Inputs to one [`TaskTiming::evaluate`] call. Defaults are the unblocked,
/// never-fired, unpaused case, so a row states only what it is testing.
#[derive(Default)]
struct Timing {
    blocked: Option<crate::trust::TrustState>,
    last_fire: Option<Timestamp>,
    pause: Option<pauses::PauseEntry>,
}

impl Timing {
    /// A task last fired `ago` seconds before the evaluation instant.
    fn fired(ago: i64, now: &Zoned) -> Self {
        Self {
            last_fire: Some(before(now, ago)),
            ..Self::default()
        }
    }

    fn blocked(self, state: crate::trust::TrustState) -> Self {
        Self {
            blocked: Some(state),
            ..self
        }
    }

    fn paused(self, pause: pauses::PauseEntry) -> Self {
        Self {
            pause: Some(pause),
            ..self
        }
    }

    fn state(self, entry: &TaskEntry, now: &Zoned) -> TaskTimingState {
        TaskTiming::evaluate(
            &parse_schedule("task", entry),
            self.blocked,
            self.last_fire,
            self.pause.as_ref(),
            now,
        )
        .state()
    }
}

/// Display state resolves blocked ▸ active pause ▸ schedule, and an ended pause
/// becomes the effective last-fire edge so a resumed task does not replay.
#[test]
fn task_timing_state_precedence_and_classification() {
    use TaskTimingState::{Blocked, Due, Invalid, NoOccurrence, Paused, Unarmed, Upcoming};

    let now = zdt(2026, 6, 24, 8, 10, 0);
    let interval = entry(None, Some("15m"), None);
    let invalid = entry(None, None, None);
    let stale = crate::trust::TrustState::Stale;
    let manual = pauses::PauseEntry {
        until: None,
        strikes: Some(3),
    };
    let timed = pauses::PauseEntry {
        until: Some(after(&now, 5 * 60)),
        strikes: None,
    };
    let ended = pauses::PauseEntry {
        until: Some(before(&now, 5 * 60)),
        strikes: None,
    };

    // Every overlay row carries an unparseable entry, proving the overlay wins
    // over whatever the schedule half would have said.
    let overlay = |timing: Timing| timing.state(&invalid, &now);
    assert_eq!(
        overlay(Timing::default().blocked(stale).paused(manual)),
        Blocked(stale)
    );
    assert_eq!(overlay(Timing::default().paused(manual)), Paused(manual));
    assert_eq!(overlay(Timing::default().paused(timed)), Paused(timed));
    assert_eq!(overlay(Timing::fired(0, &now)), Invalid);

    assert_eq!(Timing::default().state(&interval, &now), Unarmed);
    assert_eq!(
        Timing::fired(10 * 60, &now).state(&interval, &now),
        Upcoming(after(&now, 5 * 60)),
    );
    // An ended pause moves the edge forward: 20 minutes since the last fire, but
    // the pause lifted 5 minutes ago, so the next 15m occurrence is 10m out.
    assert_eq!(
        Timing::fired(20 * 60, &now)
            .paused(ended)
            .state(&interval, &now),
        Upcoming(after(&now, 10 * 60)),
    );
    // A sparse cron expression outside the bounded search has no computable
    // occurrence.
    assert_eq!(
        Timing::fired(60, &zdt(2026, 1, 2, 0, 0, 0)).state(
            &entry(None, None, Some("0 0 1 1 *")),
            &zdt(2026, 1, 2, 0, 0, 0)
        ),
        NoOccurrence,
    );

    // Every schedule variant reaches Due through `next_after`, agreeing with the
    // `due` predicate that `schedule_due_at_occurrence_edges` pins directly.
    for (label, entry, timing) in [
        (
            "calendar",
            entry(Some("08:10"), None, None),
            Timing::fired(60, &now),
        ),
        (
            "interval",
            entry(None, Some("15m"), None),
            Timing::fired(15 * 60, &now),
        ),
        (
            "cron",
            entry(None, None, Some("10 8 * * *")),
            Timing::fired(60, &now),
        ),
    ] {
        assert!(matches!(timing.state(&entry, &now), Due(_)), "{label}");
    }
}
