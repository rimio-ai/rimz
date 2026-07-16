//! Scheduled loop task core.
//!
//! The elected sidebar elder keeps time for loop tasks while a room is open and
//! fires `rimz loop run <name>`, which drives one configured loop wake-up. A
//! `<kind>-ping` virtual cell is the window-priming special case; the schedule
//! machinery stays generic by evaluating an externally resolved reset instant.
//!
//! This module owns task action validation, catalog precedence and mutation,
//! the [`runner::TaskFire`] state machine, terminal transitions, schedule
//! parsing, and due evaluation. CLI handlers translate flags, execute prepared
//! effects, orchestrate terminal actions, and render.

use std::time::Duration;

use crate::config::{TaskEntry, TaskTarget};
use jiff::{SignedDuration, Timestamp, Zoned};

pub mod catalog;
pub mod config_edit;
pub(crate) mod fire;
pub mod instances;
pub mod pauses;
pub mod run_log;
pub mod runner;
pub mod strikes;

pub use fire::last_stamps;

/// Executable action encoded by one loop task entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskAction<'a> {
    Spawn(&'a str),
    Deliver(&'a TaskTarget),
    CheckOnly,
}

/// Invalid combinations of loop task action fields.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskActionErr {
    #[error(
        "loop task `{name}` sets `verify` without `agent`; verification needs a supervised agent run"
    )]
    VerifyWithoutAgent { name: String },
    #[error("loop task `{name}` sets `max-attempts` without `verify`")]
    AttemptsWithoutVerify { name: String },
    #[error("loop task `{name}` sets `max-attempts` to 0; use at least 1")]
    ZeroAttempts { name: String },
    #[error("loop task `{name}` sets both `agent` and `wake`; keep exactly one")]
    ConflictingActions { name: String },
    #[error("loop task `{name}` needs `agent`, `wake`, or `check`")]
    MissingAction { name: String },
}

impl<'a> TaskAction<'a> {
    pub fn from_entry(name: &str, entry: &'a TaskEntry) -> Result<Self, TaskActionErr> {
        if entry.verify.is_some() && entry.agent.is_none() {
            return Err(TaskActionErr::VerifyWithoutAgent {
                name: name.to_owned(),
            });
        }
        if entry.max_attempts.is_some() && entry.verify.is_none() {
            return Err(TaskActionErr::AttemptsWithoutVerify {
                name: name.to_owned(),
            });
        }
        if entry.max_attempts == Some(0) {
            return Err(TaskActionErr::ZeroAttempts {
                name: name.to_owned(),
            });
        }
        match (entry.agent.as_deref(), entry.wake.as_ref()) {
            (Some(agent), None) if !agent.trim().is_empty() => Ok(Self::Spawn(agent)),
            (None, Some(target)) => Ok(Self::Deliver(target)),
            (None, None) if entry.check.is_some() => Ok(Self::CheckOnly),
            (Some(_), Some(_)) => Err(TaskActionErr::ConflictingActions {
                name: name.to_owned(),
            }),
            _ => Err(TaskActionErr::MissingAction {
                name: name.to_owned(),
            }),
        }
    }

    pub fn subject(self) -> &'a str {
        match self {
            Self::Spawn(spec) => spec,
            Self::Deliver(target) => &target.handle,
            Self::CheckOnly => "check",
        }
    }

    pub const fn is_check_only(self) -> bool {
        matches!(self, Self::CheckOnly)
    }
}

/// Errors from parsing or validating a schedule entry.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleErr {
    #[error(
        "schedule `{name}` needs a firing time: set `at = \"HH:MM\"` for a one-shot, `every = \"30m\"`, or `cron = \"...\"`"
    )]
    NoTime { name: String },
    #[error(
        "schedule `{name}` sets conflicting schedule fields; use `cron`, `every`, or bare `at`"
    )]
    TimeConflict { name: String },
    #[error(
        "schedule `{name}` sets `every = \"reset\"`, which only applies to a `<kind>-ping` agent task"
    )]
    ResetNeedsPing { name: String },
    #[error("schedule `{name}` sets a calendar `every` value without `at`; add `at = \"HH:MM\"`")]
    EveryNeedsAt { name: String },
    #[error("schedule `{name}` has an invalid time `{value}`; use 24-hour `HH:MM`")]
    BadTime { name: String, value: String },
    #[error("schedule `{name}` has an invalid cron expression `{value}`; expected 5 fields")]
    BadCron { name: String, value: String },
    #[error(
        "schedule `{name}` has an invalid `every` value `{value}`; use `reset`, a duration like `30m`, or a day mask like `weekday` or `mon,wed,fri`"
    )]
    BadEvery { name: String, value: String },
    #[error("schedule name `{name}` must be non-empty and use only letters, digits, `-`, or `_`")]
    BadName { name: String },
}

/// A weekday in Mon..Sun order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    const ORDER: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    fn index(self) -> usize {
        Self::ORDER
            .iter()
            .position(|d| *d == self)
            .expect("weekday in order")
    }

    fn short_name(self) -> &'static str {
        match self {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        }
    }

    fn parse(token: &str) -> Option<Weekday> {
        match token.trim().to_ascii_lowercase().as_str() {
            "mon" | "monday" => Some(Weekday::Mon),
            "tue" | "tues" | "tuesday" => Some(Weekday::Tue),
            "wed" | "weds" | "wednesday" => Some(Weekday::Wed),
            "thu" | "thur" | "thurs" | "thursday" => Some(Weekday::Thu),
            "fri" | "friday" => Some(Weekday::Fri),
            "sat" | "saturday" => Some(Weekday::Sat),
            "sun" | "sunday" => Some(Weekday::Sun),
            _ => None,
        }
    }
}

/// A normalized firing time: minute, hour (local wall-clock), and the weekday
/// set. An empty weekday set means every day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarSpec {
    pub minute: u8,
    pub hour: u8,
    pub weekdays: Vec<Weekday>,
}

/// A normalized interval schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntervalSpec {
    pub minutes: u32,
}

impl IntervalSpec {
    fn new(minutes: u32) -> Self {
        Self { minutes }
    }
}

/// A parsed schedule: calendar time, interval, or a raw cron escape hatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    Calendar(CalendarSpec),
    Interval(IntervalSpec),
    RawCron(String),
    WindowReset,
}

/// Skew margin so a reset-priming ping lands in the new provider window, never
/// the final seconds of the old one.
pub const RESET_PING_MARGIN: SignedDuration = SignedDuration::from_secs(60);

impl Schedule {
    /// A short human description for listings.
    pub fn describe(&self) -> String {
        match self {
            Schedule::RawCron(cron) => format!("cron `{cron}`"),
            Schedule::WindowReset => "every window reset".to_owned(),
            Schedule::Calendar(spec) => {
                let days = if spec.weekdays.is_empty() {
                    "day".to_owned()
                } else {
                    spec.weekdays
                        .iter()
                        .map(|d| d.short_name())
                        .collect::<Vec<_>>()
                        .join(",")
                };
                format!("every {days} at {:02}:{:02}", spec.hour, spec.minute)
            }
            Schedule::Interval(spec) => format!("every {}", format_minutes(spec.minutes)),
        }
    }

    /// Whether this schedule is due now, given the last time its task was
    /// armed or fired. First-sight arming is owned by the elder firing module.
    pub fn due(&self, last_fire: Timestamp, now: &Zoned, window_reset: Option<Timestamp>) -> bool {
        match self {
            Schedule::Interval(spec) => {
                now.timestamp().duration_since(last_fire).as_secs() >= i64::from(spec.minutes) * 60
            }
            Schedule::Calendar(spec) => calendar_due(spec, last_fire, now),
            Schedule::RawCron(expr) => {
                cron_matches(expr, now) && minute_bucket(last_fire) < minute_bucket(now.timestamp())
            }
            Schedule::WindowReset => window_reset_due(last_fire, now.timestamp(), window_reset),
        }
    }

    /// First occurrence after `last_fire`, evaluated from `now` in the
    /// configured local zone. A returned timestamp may be at or before `now`,
    /// which means the elder should fire on its next tick.
    pub fn next_after(
        &self,
        last_fire: Timestamp,
        now: &Zoned,
        window_reset: Option<Timestamp>,
    ) -> Option<Timestamp> {
        match self {
            Schedule::Interval(spec) => last_fire
                .checked_add(SignedDuration::from_secs(i64::from(spec.minutes) * 60))
                .ok(),
            Schedule::Calendar(spec) => calendar_next_after(spec, last_fire, now),
            Schedule::RawCron(expr) => cron_next_after(expr, last_fire, now),
            Schedule::WindowReset => window_reset_next_after(last_fire, window_reset),
        }
    }
}

/// A parsed task schedule plus its one-shot flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSchedule {
    pub schedule: Schedule,
    pub once: bool,
}

impl ParsedSchedule {
    /// A short human description for listings.
    pub fn describe(&self) -> String {
        if self.once
            && let Schedule::Calendar(spec) = &self.schedule
        {
            return format!("once at {:02}:{:02}", spec.hour, spec.minute);
        }
        self.schedule.describe()
    }
}

/// One task's parsed schedule and current display timing classification.
#[derive(Debug)]
pub struct TaskTiming {
    parsed: Result<ParsedSchedule, ScheduleErr>,
    state: TaskTimingState,
    active_pause: Option<pauses::PauseEntry>,
}

/// Current schedule state before CLI presentation and live run-lock overlays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskTimingState {
    Blocked(crate::trust::TrustState),
    Paused(pauses::PauseEntry),
    Invalid,
    Unarmed,
    Upcoming(Timestamp),
    Due(Timestamp),
    NoOccurrence,
}

impl TaskTiming {
    pub fn evaluate(
        name: &str,
        entry: &TaskEntry,
        blocked: Option<crate::trust::TrustState>,
        last_fire: Option<Timestamp>,
        pause: Option<&pauses::PauseEntry>,
        now: &Zoned,
        window_reset: Option<Timestamp>,
    ) -> Self {
        let parsed = parse_schedule(name, entry);
        let active_pause = pause
            .filter(|pause| pauses::is_active(pause, now.timestamp()))
            .copied();
        let state = if let Some(state) = blocked {
            TaskTimingState::Blocked(state)
        } else if let Some(pause) = active_pause {
            TaskTimingState::Paused(pause)
        } else {
            match (&parsed, last_fire) {
                (Err(_), _) => TaskTimingState::Invalid,
                (Ok(_), None) => TaskTimingState::Unarmed,
                (Ok(parsed), Some(last_fire)) => {
                    let effective_last_fire =
                        pauses::effective_last_fire(last_fire, pause, now.timestamp());
                    match parsed
                        .schedule
                        .next_after(effective_last_fire, now, window_reset)
                    {
                        Some(next) if next <= now.timestamp() => TaskTimingState::Due(next),
                        Some(next) => TaskTimingState::Upcoming(next),
                        None => TaskTimingState::NoOccurrence,
                    }
                }
            }
        };
        Self {
            parsed,
            state,
            active_pause,
        }
    }

    pub const fn state(&self) -> TaskTimingState {
        self.state
    }

    pub fn parsed(&self) -> Result<&ParsedSchedule, &ScheduleErr> {
        self.parsed.as_ref()
    }

    pub const fn active_pause(&self) -> Option<pauses::PauseEntry> {
        self.active_pause
    }

    pub const fn next_timestamp(&self) -> Option<Timestamp> {
        match self.state {
            TaskTimingState::Upcoming(next) | TaskTimingState::Due(next) => Some(next),
            _ => None,
        }
    }
}

/// Validate a schedule name: non-empty and limited to a filesystem- and
/// shell-safe charset.
pub fn validate_name(name: &str) -> Result<(), ScheduleErr> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if ok {
        Ok(())
    } else {
        Err(ScheduleErr::BadName {
            name: name.to_owned(),
        })
    }
}

/// Parse `<n><unit>` against an allowed-units slice. Each entry is
/// `(unit_str, multiplier_in_seconds)`.
pub fn parse_duration_units(raw: &str, allowed: &[(&str, u64)]) -> Result<Duration, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("duration is empty".to_owned());
    }
    let (digits, unit) = trimmed
        .split_at_checked(trimmed.len() - 1)
        .ok_or_else(|| format!("unrecognised duration `{raw}`"))?;
    let factor = allowed
        .iter()
        .find_map(|(name, mult)| (*name == unit).then_some(*mult))
        .ok_or_else(|| {
            let units = allowed
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join("/");
            format!("unknown duration unit `{unit}`; use {units}")
        })?;
    let n: u64 = digits
        .parse()
        .map_err(|e| format!("duration `{raw}` is not an integer: {e}"))?;
    Ok(Duration::from_secs(n.saturating_mul(factor)))
}

/// Parse a positive forward-headroom ratio such as `1.5x`.
pub fn parse_surplus(raw: &str) -> Result<f64, String> {
    let trimmed = raw.trim();
    let number = trimmed.strip_suffix(['x', 'X']).unwrap_or(trimmed).trim();
    let ratio = number
        .parse::<f64>()
        .map_err(|err| format!("surplus ratio `{raw}` is not a number: {err}"))?;
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(format!(
            "surplus ratio `{raw}` must be finite and greater than zero"
        ));
    }
    Ok(ratio)
}

/// Parse the elapsed floor for a provider budget-window surplus gate.
pub fn parse_surplus_after(raw: &str) -> Result<Duration, String> {
    parse_duration_units(raw, &[("m", 60), ("h", 3_600), ("d", 86_400)])
}

/// Parse and validate an entry's firing time into a [`ParsedSchedule`]. Full
/// agent preflight is validated separately by the CLI.
pub fn parse_schedule(name: &str, entry: &TaskEntry) -> Result<ParsedSchedule, ScheduleErr> {
    if entry.cron.is_some() && (entry.at.is_some() || entry.every.is_some()) {
        return Err(ScheduleErr::TimeConflict {
            name: name.to_owned(),
        });
    }
    let schedule = match (
        entry.cron.as_deref(),
        entry.every.as_deref(),
        entry.at.as_deref(),
    ) {
        (Some(cron), None, None) => {
            validate_cron_expr(name, cron)?;
            Schedule::RawCron(cron.trim().to_owned())
        }
        (None, Some(every), at) => match parse_every(name, every)? {
            EverySpec::Reset => {
                if at.is_some() {
                    return Err(ScheduleErr::TimeConflict {
                        name: name.to_owned(),
                    });
                }
                if entry
                    .agent
                    .as_deref()
                    .is_some_and(crate::harness::spec::virtual_ping_shape)
                {
                    Schedule::WindowReset
                } else {
                    return Err(ScheduleErr::ResetNeedsPing {
                        name: name.to_owned(),
                    });
                }
            }
            EverySpec::Interval(minutes) => {
                if at.is_some() {
                    return Err(ScheduleErr::TimeConflict {
                        name: name.to_owned(),
                    });
                }
                Schedule::Interval(IntervalSpec::new(minutes))
            }
            EverySpec::Days(weekdays) => {
                let at = at.ok_or_else(|| ScheduleErr::EveryNeedsAt {
                    name: name.to_owned(),
                })?;
                let (hour, minute) = parse_task_hhmm(name, at)?;
                Schedule::Calendar(CalendarSpec {
                    minute,
                    hour,
                    weekdays,
                })
            }
        },
        (None, None, Some(at)) => {
            let (hour, minute) = parse_task_hhmm(name, at)?;
            Schedule::Calendar(CalendarSpec {
                minute,
                hour,
                weekdays: Vec::new(),
            })
        }
        (None, None, None) => {
            return Err(ScheduleErr::NoTime {
                name: name.to_owned(),
            });
        }
        _ => unreachable!("cron conflicts returned before parsing"),
    };
    Ok(ParsedSchedule {
        schedule,
        once: entry.every.is_none() && entry.cron.is_none(),
    })
}

enum EverySpec {
    Interval(u32),
    Days(Vec<Weekday>),
    Reset,
}

fn parse_every(name: &str, raw: &str) -> Result<EverySpec, ScheduleErr> {
    let value = raw.trim();
    if raw == "reset" {
        return Ok(EverySpec::Reset);
    }
    if let Ok(minutes) = parse_interval_minutes(raw) {
        return Ok(EverySpec::Interval(minutes));
    }
    if let Some(days) = parse_days(value) {
        return Ok(EverySpec::Days(days));
    }
    Err(ScheduleErr::BadEvery {
        name: name.to_owned(),
        value: raw.to_owned(),
    })
}

fn parse_task_hhmm(name: &str, value: &str) -> Result<(u8, u8), ScheduleErr> {
    let bad = || ScheduleErr::BadTime {
        name: name.to_owned(),
        value: value.to_owned(),
    };
    parse_hhmm(value).ok_or_else(bad)
}

pub fn parse_hhmm(value: &str) -> Option<(u8, u8)> {
    let (hh, mm) = value.trim().split_once(':')?;
    let hour: u8 = hh.parse().ok()?;
    let minute: u8 = mm.parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

fn parse_days(days: &str) -> Option<Vec<Weekday>> {
    let days = days.trim();
    if days.is_empty() {
        return None;
    }
    match days.to_ascii_lowercase().as_str() {
        "day" | "daily" => return Some(Vec::new()),
        "weekday" | "weekdays" => return Some(weekday_range(Weekday::Mon, Weekday::Fri)),
        "weekend" | "weekends" => return Some(vec![Weekday::Sat, Weekday::Sun]),
        _ => {}
    }
    let mut set: Vec<Weekday> = Vec::new();
    for token in days.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        let expanded = if let Some((lo, hi)) = token.split_once('-') {
            let lo = Weekday::parse(lo)?;
            let hi = Weekday::parse(hi)?;
            weekday_range(lo, hi)
        } else {
            vec![Weekday::parse(token)?]
        };
        for day in expanded {
            if !set.contains(&day) {
                set.push(day);
            }
        }
    }
    set.sort();
    Some(set)
}

fn parse_interval_minutes(raw: &str) -> Result<u32, ()> {
    let value = raw.trim();
    if value.len() < 2 {
        return Err(());
    }
    let unit = value.chars().last().ok_or(())?;
    let digits = &value[..value.len() - unit.len_utf8()];
    let amount: u64 = digits.parse().map_err(|_| ())?;
    if amount == 0 {
        return Err(());
    }
    let seconds = match unit {
        's' => amount,
        'm' => amount.checked_mul(60).ok_or(())?,
        'h' => amount.checked_mul(60 * 60).ok_or(())?,
        'd' => amount.checked_mul(24 * 60 * 60).ok_or(())?,
        _ => return Err(()),
    };
    let minutes = seconds.div_ceil(60);
    u32::try_from(minutes).map_err(|_| ())
}

/// Inclusive Mon..Sun range; a wrap-around (e.g. `fri-mon`) walks forward to Sun.
fn weekday_range(lo: Weekday, hi: Weekday) -> Vec<Weekday> {
    let (lo, hi) = (lo.index(), hi.index());
    if lo <= hi {
        Weekday::ORDER[lo..=hi].to_vec()
    } else {
        Weekday::ORDER[lo..]
            .iter()
            .chain(&Weekday::ORDER[..=hi])
            .copied()
            .collect()
    }
}

fn validate_cron_expr(name: &str, cron: &str) -> Result<(), ScheduleErr> {
    let fields = cron.split_whitespace().count();
    if fields == 5 {
        Ok(())
    } else {
        Err(ScheduleErr::BadCron {
            name: name.to_owned(),
            value: cron.to_owned(),
        })
    }
}

fn format_minutes(minutes: u32) -> String {
    if minutes >= 1440 && minutes.is_multiple_of(1440) {
        format!("{}d", minutes / 1440)
    } else if minutes >= 60 && minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn window_reset_occurrence(window_reset: Option<Timestamp>) -> Option<Timestamp> {
    window_reset?.checked_add(RESET_PING_MARGIN).ok()
}

fn window_reset_due(last_fire: Timestamp, now: Timestamp, window_reset: Option<Timestamp>) -> bool {
    window_reset_occurrence(window_reset)
        .is_some_and(|occurrence| last_fire < occurrence && now >= occurrence)
}

fn window_reset_next_after(
    last_fire: Timestamp,
    window_reset: Option<Timestamp>,
) -> Option<Timestamp> {
    window_reset_occurrence(window_reset).filter(|occurrence| *occurrence > last_fire)
}

fn calendar_due(spec: &CalendarSpec, last_fire: Timestamp, now: &Zoned) -> bool {
    if !spec.weekdays.is_empty() && !spec.weekdays.contains(&weekday_from_jiff(now.weekday())) {
        return false;
    }
    if (now.hour(), now.minute()) < (spec.hour as i8, spec.minute as i8) {
        return false;
    }
    let Ok(occurrence) = now
        .date()
        .at(spec.hour as i8, spec.minute as i8, 0, 0)
        .to_zoned(now.time_zone().clone())
    else {
        return false;
    };
    last_fire < occurrence.timestamp() && now.timestamp() >= occurrence.timestamp()
}

fn calendar_next_after(
    spec: &CalendarSpec,
    last_fire: Timestamp,
    now: &Zoned,
) -> Option<Timestamp> {
    for days in 0..=8 {
        let date = now
            .date()
            .checked_add(Duration::from_secs(days * 86_400))
            .ok()?;
        if !spec.weekdays.is_empty() && !spec.weekdays.contains(&weekday_from_jiff(date.weekday()))
        {
            continue;
        }
        let Ok(occurrence) = date
            .at(spec.hour as i8, spec.minute as i8, 0, 0)
            .to_zoned(now.time_zone().clone())
        else {
            continue;
        };
        let timestamp = occurrence.timestamp();
        if timestamp > last_fire {
            return Some(timestamp);
        }
    }
    None
}

fn weekday_from_jiff(day: jiff::civil::Weekday) -> Weekday {
    match day {
        jiff::civil::Weekday::Monday => Weekday::Mon,
        jiff::civil::Weekday::Tuesday => Weekday::Tue,
        jiff::civil::Weekday::Wednesday => Weekday::Wed,
        jiff::civil::Weekday::Thursday => Weekday::Thu,
        jiff::civil::Weekday::Friday => Weekday::Fri,
        jiff::civil::Weekday::Saturday => Weekday::Sat,
        jiff::civil::Weekday::Sunday => Weekday::Sun,
    }
}

fn minute_bucket(timestamp: Timestamp) -> i64 {
    timestamp.as_second().div_euclid(60)
}

fn cron_next_after(expr: &str, last_fire: Timestamp, now: &Zoned) -> Option<Timestamp> {
    if cron_matches(expr, now) && minute_bucket(last_fire) < minute_bucket(now.timestamp()) {
        return Some(now.timestamp());
    }
    let start_bucket = minute_bucket(now.timestamp()) + 1;
    let max_minutes = 60 * 24 * 60;
    for offset in 0..=max_minutes {
        let second = start_bucket.checked_add(offset)?.checked_mul(60)?;
        let timestamp = Timestamp::from_second(second).ok()?;
        let candidate = timestamp.to_zoned(now.time_zone().clone());
        if cron_matches(expr, &candidate) {
            return Some(timestamp);
        }
    }
    None
}

fn cron_matches(expr: &str, now: &Zoned) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    let [minute, hour, day_of_month, month, day_of_week] = fields.as_slice() else {
        return false;
    };
    let minute_match = cron_field_matches(minute, now.minute(), 0, 59);
    let hour_match = cron_field_matches(hour, now.hour(), 0, 23);
    let dom_match = cron_field_matches(day_of_month, now.day(), 1, 31);
    let month_match = cron_field_matches(month, now.month(), 1, 12);
    let dow_match = cron_dow_matches(day_of_week, now.weekday());
    let dom_restricted = day_of_month.trim() != "*";
    let dow_restricted = day_of_week.trim() != "*";
    let day_match = match (dom_restricted, dow_restricted) {
        (true, true) => dom_match || dow_match,
        (true, false) => dom_match,
        (false, true) => dow_match,
        (false, false) => true,
    };
    minute_match && hour_match && month_match && day_match
}

fn cron_field_matches(expr: &str, value: i8, min: i8, max: i8) -> bool {
    cron_field_matches_any(expr, &[value], min, max)
}

fn cron_field_matches_any(expr: &str, values: &[i8], min: i8, max: i8) -> bool {
    expr.split(',').any(|part| {
        let part = part.trim();
        if part.is_empty() {
            return false;
        }
        if part == "*" {
            return true;
        }
        if let Some(step) = part.strip_prefix("*/") {
            let Ok(step) = step.parse::<i8>() else {
                return false;
            };
            return step > 0
                && values
                    .iter()
                    .any(|value| (*value - min).rem_euclid(step) == 0);
        }
        if let Some((start, end)) = part.split_once('-') {
            let Some(start) = parse_cron_value(start, min, max) else {
                return false;
            };
            let Some(end) = parse_cron_value(end, min, max) else {
                return false;
            };
            return start <= end && values.iter().any(|value| start <= *value && *value <= end);
        }
        parse_cron_value(part, min, max).is_some_and(|parsed| values.contains(&parsed))
    })
}

fn parse_cron_value(raw: &str, min: i8, max: i8) -> Option<i8> {
    let value: i8 = raw.trim().parse().ok()?;
    (min..=max).contains(&value).then_some(value)
}

fn cron_dow_matches(expr: &str, day: jiff::civil::Weekday) -> bool {
    match day {
        jiff::civil::Weekday::Sunday => cron_field_matches_any(expr, &[0, 7], 0, 7),
        jiff::civil::Weekday::Monday => cron_field_matches(expr, 1, 0, 7),
        jiff::civil::Weekday::Tuesday => cron_field_matches(expr, 2, 0, 7),
        jiff::civil::Weekday::Wednesday => cron_field_matches(expr, 3, 0, 7),
        jiff::civil::Weekday::Thursday => cron_field_matches(expr, 4, 0, 7),
        jiff::civil::Weekday::Friday => cron_field_matches(expr, 5, 0, 7),
        jiff::civil::Weekday::Saturday => cron_field_matches(expr, 6, 0, 7),
    }
}

#[cfg(test)]
mod tests;
