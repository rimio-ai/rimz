//! Scheduled loop task core.
//!
//! Rimz keeps no daemon: the OS scheduler keeps time and fires
//! `rimz loop run <name>`, which drives one supervised agent turn through the
//! existing agent harness. A `<kind>-ping` virtual cell is the window-priming
//! special case; the schedule machinery stays generic.
//!
//! This module is the pure core. It normalizes a [`crate::config::TaskEntry`]
//! into a [`Schedule`], renders the cron line and systemd units, builds the
//! login-shell command the scheduler runs, and owns the marker-fenced, idempotent
//! crontab reclaim. The side-effecting install/uninstall glue (writing units,
//! `systemctl --user`, reading/writing the crontab) lives in the CLI handler.

use std::path::Path;

use crate::config::TaskEntry;

/// Errors from parsing or validating a schedule entry.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleErr {
    #[error(
        "schedule `{name}` needs a firing time: set `at = \"HH:MM\"`, `every = \"30m\"`, or `cron = \"...\"`"
    )]
    NoTime { name: String },
    #[error("schedule `{name}` sets more than one of `at`/`days`, `every`, and `cron`; use one")]
    TimeConflict { name: String },
    #[error("schedule `{name}` has an invalid time `{value}`; use 24-hour `HH:MM`")]
    BadTime { name: String, value: String },
    #[error(
        "schedule `{name}` has an unknown day `{value}`; use mon..sun, a range like `mon-fri`, `daily`, `weekdays`, or `weekends`"
    )]
    BadDay { name: String, value: String },
    #[error("schedule `{name}` has an invalid cron expression `{value}`; expected 5 fields")]
    BadCron { name: String, value: String },
    #[error(
        "schedule `{name}` has an invalid interval `{value}`; use a duration like `30m`, `2h`, or `1d`"
    )]
    BadInterval { name: String, value: String },
    #[error("schedule `{name}` sets both `once` and `every`; one-shot intervals are contradictory")]
    OnceWithInterval { name: String },
    #[error("schedule `{name}` uses a raw `cron` expression, which only the cron backend supports")]
    RawCronOnSystemd { name: String },
    #[error("schedule name `{name}` must be non-empty and use only letters, digits, `-`, or `_`")]
    BadName { name: String },
}

/// The OS scheduler an install targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheduler {
    /// A systemd user timer (`~/.config/systemd/user/`), driven by `systemctl --user`.
    SystemdUser,
    /// The user crontab, edited through `crontab -l` / `crontab -`.
    Cron,
}

impl Scheduler {
    pub fn label(self) -> &'static str {
        match self {
            Scheduler::SystemdUser => "systemd user timer",
            Scheduler::Cron => "crontab",
        }
    }
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

    /// The cron day-of-week number: Sun=0, Mon=1, … Sat=6.
    fn cron_num(self) -> u8 {
        match self {
            Weekday::Sun => 0,
            Weekday::Mon => 1,
            Weekday::Tue => 2,
            Weekday::Wed => 3,
            Weekday::Thu => 4,
            Weekday::Fri => 5,
            Weekday::Sat => 6,
        }
    }

    fn systemd_name(self) -> &'static str {
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
    cron_minutes: u32,
}

impl IntervalSpec {
    fn new(minutes: u32) -> Self {
        Self {
            minutes,
            cron_minutes: cron_interval_minutes(minutes),
        }
    }

    fn cron_note(&self) -> Option<String> {
        (self.minutes != self.cron_minutes).then(|| {
            format!(
                " (cron rounds to every {})",
                format_minutes(self.cron_minutes)
            )
        })
    }
}

/// A parsed schedule: calendar time, interval, or a raw cron escape hatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    Calendar(CalendarSpec),
    Interval(IntervalSpec),
    RawCron(String),
}

impl Schedule {
    /// A short human description for listings and the install preview.
    pub fn describe(&self) -> String {
        match self {
            Schedule::RawCron(cron) => format!("cron `{cron}`"),
            Schedule::Calendar(spec) => {
                let days = if spec.weekdays.is_empty() {
                    "every day".to_owned()
                } else {
                    spec.weekdays
                        .iter()
                        .map(|d| d.systemd_name())
                        .collect::<Vec<_>>()
                        .join(",")
                };
                format!("{:02}:{:02} {days}", spec.hour, spec.minute)
            }
            Schedule::Interval(spec) => {
                let note = spec.cron_note().unwrap_or_default();
                format!("every {}{note}", format_minutes(spec.minutes))
            }
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
    /// A short human description for listings and the install preview.
    pub fn describe(&self) -> String {
        let description = self.schedule.describe();
        if self.once {
            format!("once {description}")
        } else {
            description
        }
    }
}

/// The crontab fence that marks a rimz-owned block: a `# rimz-loop:<name>`
/// comment line followed by the command line, so reclaim is exact and never
/// touches a user's own crontab lines.
pub const CRON_TAG_PREFIX: &str = "# rimz-loop:";

/// One rimz-owned crontab block: its schedule name and command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronEntry {
    pub name: String,
    pub line: String,
}

/// Validate a schedule name: non-empty and limited to a filesystem- and
/// shell-safe charset, so it is safe both as a systemd unit stem and inside the
/// crontab command without quoting.
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

/// Parse and validate an entry's firing time into a [`ParsedSchedule`]. Agent
/// support is validated separately at run/install time, where the adapter
/// registry is in scope.
pub fn parse_schedule(name: &str, entry: &TaskEntry) -> Result<ParsedSchedule, ScheduleErr> {
    let has_calendar = entry.at.is_some() || entry.days.is_some();
    let set_count = usize::from(has_calendar)
        + usize::from(entry.every.is_some())
        + usize::from(entry.cron.is_some());
    if set_count > 1 {
        return Err(ScheduleErr::TimeConflict {
            name: name.to_owned(),
        });
    }
    if entry.once && entry.every.is_some() {
        return Err(ScheduleErr::OnceWithInterval {
            name: name.to_owned(),
        });
    }
    let schedule = match (entry.cron.as_deref(), entry.every.as_deref(), has_calendar) {
        (Some(cron), None, false) => {
            validate_cron_expr(name, cron)?;
            Schedule::RawCron(cron.trim().to_owned())
        }
        (None, Some(every), false) => {
            Schedule::Interval(IntervalSpec::new(parse_interval_minutes(name, every)?))
        }
        (None, None, true) => {
            let at = entry.at.as_deref().ok_or_else(|| ScheduleErr::NoTime {
                name: name.to_owned(),
            })?;
            let (hour, minute) = parse_hhmm(name, at)?;
            let weekdays = parse_days(name, entry.days.as_deref())?;
            Schedule::Calendar(CalendarSpec {
                minute,
                hour,
                weekdays,
            })
        }
        (None, None, false) => {
            return Err(ScheduleErr::NoTime {
                name: name.to_owned(),
            });
        }
        _ => unreachable!("conflicts returned before parsing"),
    };
    Ok(ParsedSchedule {
        schedule,
        once: entry.once,
    })
}

fn parse_hhmm(name: &str, value: &str) -> Result<(u8, u8), ScheduleErr> {
    let bad = || ScheduleErr::BadTime {
        name: name.to_owned(),
        value: value.to_owned(),
    };
    let (hh, mm) = value.trim().split_once(':').ok_or_else(bad)?;
    let hour: u8 = hh.parse().map_err(|_| bad())?;
    let minute: u8 = mm.parse().map_err(|_| bad())?;
    if hour > 23 || minute > 59 {
        return Err(bad());
    }
    Ok((hour, minute))
}

fn parse_days(name: &str, days: Option<&str>) -> Result<Vec<Weekday>, ScheduleErr> {
    let Some(days) = days.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(Vec::new());
    };
    let bad = |value: &str| ScheduleErr::BadDay {
        name: name.to_owned(),
        value: value.to_owned(),
    };
    match days.to_ascii_lowercase().as_str() {
        "daily" | "every" | "everyday" | "all" | "*" => return Ok(Vec::new()),
        "weekdays" => return Ok(weekday_range(Weekday::Mon, Weekday::Fri)),
        "weekends" => return Ok(vec![Weekday::Sat, Weekday::Sun]),
        _ => {}
    }
    let mut set: Vec<Weekday> = Vec::new();
    for token in days.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err(bad(days));
        }
        let expanded = if let Some((lo, hi)) = token.split_once('-') {
            let lo = Weekday::parse(lo).ok_or_else(|| bad(token))?;
            let hi = Weekday::parse(hi).ok_or_else(|| bad(token))?;
            weekday_range(lo, hi)
        } else {
            vec![Weekday::parse(token).ok_or_else(|| bad(token))?]
        };
        for day in expanded {
            if !set.contains(&day) {
                set.push(day);
            }
        }
    }
    set.sort();
    Ok(set)
}

fn parse_interval_minutes(name: &str, raw: &str) -> Result<u32, ScheduleErr> {
    let value = raw.trim();
    let bad = || ScheduleErr::BadInterval {
        name: name.to_owned(),
        value: raw.to_owned(),
    };
    if value.len() < 2 {
        return Err(bad());
    }
    let unit = value.chars().last().ok_or_else(bad)?;
    let digits = &value[..value.len() - unit.len_utf8()];
    let amount: u64 = digits.parse().map_err(|_| bad())?;
    if amount == 0 {
        return Err(bad());
    }
    let seconds = match unit {
        's' => amount,
        'm' => amount.checked_mul(60).ok_or_else(bad)?,
        'h' => amount.checked_mul(60 * 60).ok_or_else(bad)?,
        'd' => amount.checked_mul(24 * 60 * 60).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    let minutes = seconds.div_ceil(60);
    u32::try_from(minutes).map_err(|_| bad())
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

/// The cron schedule expression (the five fields before the command).
pub fn cron_expr(schedule: &Schedule) -> String {
    match schedule {
        Schedule::RawCron(raw) => raw.clone(),
        Schedule::Interval(spec) => interval_cron_expr(spec.cron_minutes),
        Schedule::Calendar(spec) => {
            let dow = if spec.weekdays.is_empty() {
                "*".to_owned()
            } else {
                let mut nums: Vec<u8> = spec.weekdays.iter().map(|d| d.cron_num()).collect();
                nums.sort_unstable();
                nums.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
            };
            format!("{} {} * * {}", spec.minute, spec.hour, dow)
        }
    }
}

fn interval_cron_expr(minutes: u32) -> String {
    if minutes < 60 {
        format!("*/{minutes} * * * *")
    } else {
        let hours = minutes / 60;
        format!("0 */{hours} * * *")
    }
}

fn cron_interval_minutes(minutes: u32) -> u32 {
    const CLEAN: &[u32] = &[
        1, 2, 3, 4, 5, 6, 10, 12, 15, 20, 30, 60, 120, 180, 240, 360, 480, 720, 1440,
    ];
    if minutes < 60 && 60 % minutes == 0 {
        return minutes;
    }
    if minutes >= 60 && minutes.is_multiple_of(60) {
        let hours = minutes / 60;
        if hours <= 24 && 24 % hours == 0 {
            return minutes;
        }
    }
    CLEAN
        .iter()
        .copied()
        .min_by_key(|candidate| {
            let distance = candidate.abs_diff(minutes);
            (distance, *candidate < minutes)
        })
        .unwrap_or(60)
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

/// The systemd `OnCalendar=` value for a calendar schedule. Raw cron and
/// interval schedules use other timer triggers.
pub fn systemd_oncalendar(name: &str, schedule: &Schedule) -> Result<String, ScheduleErr> {
    let Schedule::Calendar(spec) = schedule else {
        return Err(ScheduleErr::RawCronOnSystemd {
            name: name.to_owned(),
        });
    };
    let days = if spec.weekdays.is_empty() {
        "*-*-*".to_owned()
    } else {
        spec.weekdays
            .iter()
            .map(|d| d.systemd_name())
            .collect::<Vec<_>>()
            .join(",")
    };
    Ok(format!("{days} {:02}:{:02}:00", spec.hour, spec.minute))
}

/// The systemd trigger for a schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemdTrigger {
    OnCalendar(String),
    OnActive { boot: String, active: String },
}

pub fn systemd_trigger(name: &str, schedule: &Schedule) -> Result<SystemdTrigger, ScheduleErr> {
    match schedule {
        Schedule::Calendar(_) => Ok(SystemdTrigger::OnCalendar(systemd_oncalendar(
            name, schedule,
        )?)),
        Schedule::Interval(spec) => {
            let minutes = format!("{}min", spec.minutes);
            Ok(SystemdTrigger::OnActive {
                boot: minutes.clone(),
                active: minutes,
            })
        }
        Schedule::RawCron(_) => Err(ScheduleErr::RawCronOnSystemd {
            name: name.to_owned(),
        }),
    }
}

/// The login-shell command an OS scheduler entry runs. Wrapping in the user's
/// login shell re-applies the interactive PATH so the mux and agent binaries
/// resolve the same way they do in a terminal; the absolute `rimz` path is baked
/// in so the entry never depends on PATH to find Rimz itself.
pub fn run_command(rimz_bin: &Path, shell: &str, name: &str) -> String {
    let inner = format!(
        "exec {} loop run {name}",
        shell_single_quote(&rimz_bin.display().to_string())
    );
    format!("{shell} -lc {}", shell_single_quote(&inner))
}

fn shell_single_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// The systemd unit file stem for a schedule: `rimz-loop-<name>`.
pub fn unit_stem(name: &str) -> String {
    format!("rimz-loop-{name}")
}

/// A one-line human description for the systemd units.
pub fn description(name: &str) -> String {
    format!("Rimz loop: {name}")
}

/// Render the `.service` oneshot unit that runs the ping command.
pub fn render_systemd_service(command: &str, description: &str) -> String {
    format!(
        "[Unit]\n\
         Description={description}\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={command}\n"
    )
}

/// Render the `.timer` unit that fires the service on the schedule.
pub fn render_systemd_timer(trigger: &SystemdTrigger, description: &str) -> String {
    let trigger = match trigger {
        SystemdTrigger::OnCalendar(oncalendar) => format!("OnCalendar={oncalendar}\n"),
        SystemdTrigger::OnActive { boot, active } => {
            format!("OnBootSec={boot}\nOnUnitActiveSec={active}\n")
        }
    };
    format!(
        "[Unit]\n\
         Description={description} timer\n\
         \n\
         [Timer]\n\
         {trigger}\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

/// Replace (or add) the rimz-owned crontab block for `name`, preserving every
/// other line. Idempotent: splicing the same name twice leaves one block.
pub fn splice_crontab(existing: &str, name: &str, line: &str) -> String {
    let mut base = reclaim_crontab(existing, Some(name));
    base.push_str(&format!("{CRON_TAG_PREFIX}{name}\n{line}\n"));
    base
}

/// Remove the rimz-owned crontab block for `name`, or every rimz-owned block
/// when `name` is `None`. A block is its `# rimz-loop:<name>` tag line plus
/// the command line that follows; foreign lines are untouched.
pub fn reclaim_crontab(existing: &str, name: Option<&str>) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut lines = existing.lines();
    while let Some(line) = lines.next() {
        if let Some(tagged) = line.strip_prefix(CRON_TAG_PREFIX) {
            let matches = name.is_none_or(|n| tagged.trim() == n);
            if matches {
                lines.next(); // drop the command line that follows the tag
                continue;
            }
        }
        out.push(line);
    }
    let mut joined = out.join("\n");
    if !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

/// The rimz-owned blocks currently in a crontab, in file order.
pub fn list_crontab(existing: &str) -> Vec<CronEntry> {
    let mut entries = Vec::new();
    let mut lines = existing.lines();
    while let Some(line) = lines.next() {
        if let Some(tagged) = line.strip_prefix(CRON_TAG_PREFIX)
            && let Some(command) = lines.next()
        {
            entries.push(CronEntry {
                name: tagged.trim().to_owned(),
                line: command.to_owned(),
            });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(
        at: Option<&str>,
        days: Option<&str>,
        every: Option<&str>,
        cron: Option<&str>,
        once: bool,
    ) -> TaskEntry {
        TaskEntry {
            spec: "claude".to_owned(),
            prompt: Some("do it".to_owned()),
            prompt_file: None,
            root: PathBuf::from("/home/me/app"),
            worktree: None,
            mode: None,
            effort: None,
            system_prompt_file: None,
            timeout: None,
            at: at.map(ToOwned::to_owned),
            days: days.map(ToOwned::to_owned),
            every: every.map(ToOwned::to_owned),
            cron: cron.map(ToOwned::to_owned),
            once,
        }
    }

    #[test]
    fn weekdays_map_to_cron_and_systemd() {
        let parsed = parse_schedule(
            "morning",
            &entry(Some("07:30"), Some("weekdays"), None, None, false),
        )
        .expect("parse");
        let schedule = parsed.schedule;
        assert_eq!(cron_expr(&schedule), "30 7 * * 1,2,3,4,5");
        assert_eq!(
            systemd_oncalendar("morning", &schedule).expect("oncalendar"),
            "Mon,Tue,Wed,Thu,Fri 07:30:00"
        );
    }

    #[test]
    fn daily_is_the_default_day_mask() {
        let from_none = parse_schedule("m", &entry(Some("07:00"), None, None, None, false))
            .expect("parse")
            .schedule;
        let from_daily =
            parse_schedule("m", &entry(Some("07:00"), Some("daily"), None, None, false))
                .expect("parse")
                .schedule;
        assert_eq!(from_none, from_daily);
        assert_eq!(cron_expr(&from_none), "0 7 * * *");
        assert_eq!(
            systemd_oncalendar("m", &from_none).expect("oncalendar"),
            "*-*-* 07:00:00"
        );
    }

    #[test]
    fn day_lists_ranges_and_weekends_parse() {
        let list = parse_schedule(
            "m",
            &entry(Some("06:05"), Some("mon,wed,fri"), None, None, false),
        )
        .expect("parse")
        .schedule;
        assert_eq!(cron_expr(&list), "5 6 * * 1,3,5");
        let range = parse_schedule(
            "m",
            &entry(Some("06:00"), Some("mon-fri"), None, None, false),
        )
        .expect("parse")
        .schedule;
        assert_eq!(cron_expr(&range), "0 6 * * 1,2,3,4,5");
        let weekends = parse_schedule(
            "m",
            &entry(Some("09:00"), Some("weekends"), None, None, false),
        )
        .expect("parse")
        .schedule;
        assert_eq!(cron_expr(&weekends), "0 9 * * 0,6");
    }

    #[test]
    fn raw_cron_passes_through_cron_and_is_rejected_by_systemd() {
        let schedule = parse_schedule("m", &entry(None, None, None, Some("0 7 * * 1-5"), false))
            .expect("parse")
            .schedule;
        assert_eq!(cron_expr(&schedule), "0 7 * * 1-5");
        assert_eq!(
            systemd_oncalendar("m", &schedule),
            Err(ScheduleErr::RawCronOnSystemd {
                name: "m".to_owned()
            })
        );
    }

    #[test]
    fn intervals_map_to_cron_and_systemd() {
        for (raw, cron) in [
            ("5m", "*/5 * * * *"),
            ("15m", "*/15 * * * *"),
            ("30m", "*/30 * * * *"),
            ("1h", "0 */1 * * *"),
            ("2h", "0 */2 * * *"),
        ] {
            let schedule = parse_schedule("m", &entry(None, None, Some(raw), None, false))
                .expect("parse")
                .schedule;
            assert_eq!(cron_expr(&schedule), cron, "{raw}");
        }

        let schedule = parse_schedule("m", &entry(None, None, Some("30m"), None, false))
            .expect("parse")
            .schedule;
        let trigger = systemd_trigger("m", &schedule).expect("trigger");
        assert_eq!(
            render_systemd_timer(&trigger, "Rimz loop: m"),
            "[Unit]\nDescription=Rimz loop: m timer\n\n[Timer]\nOnBootSec=30min\nOnUnitActiveSec=30min\n\n[Install]\nWantedBy=timers.target\n"
        );
    }

    #[test]
    fn seconds_interval_rounds_up_to_one_minute() {
        let schedule = parse_schedule("m", &entry(None, None, Some("1s"), None, false))
            .expect("parse")
            .schedule;
        assert_eq!(cron_expr(&schedule), "*/1 * * * *");
        assert_eq!(schedule.describe(), "every 1m");
    }

    #[test]
    fn conflicting_and_missing_times_error() {
        assert_eq!(
            parse_schedule(
                "m",
                &entry(Some("07:00"), None, None, Some("0 7 * * *"), false)
            ),
            Err(ScheduleErr::TimeConflict {
                name: "m".to_owned()
            })
        );
        assert_eq!(
            parse_schedule("m", &entry(None, None, None, None, false)),
            Err(ScheduleErr::NoTime {
                name: "m".to_owned()
            })
        );
        assert_eq!(
            parse_schedule("m", &entry(None, Some("weekdays"), None, None, false)),
            Err(ScheduleErr::NoTime {
                name: "m".to_owned()
            })
        );
        assert_eq!(
            parse_schedule("m", &entry(None, None, Some("5m"), None, true)),
            Err(ScheduleErr::OnceWithInterval {
                name: "m".to_owned()
            })
        );
    }

    #[test]
    fn bad_time_day_and_cron_are_rejected() {
        assert!(matches!(
            parse_schedule("m", &entry(Some("7am"), None, None, None, false)),
            Err(ScheduleErr::BadTime { .. })
        ));
        assert!(matches!(
            parse_schedule("m", &entry(Some("24:00"), None, None, None, false)),
            Err(ScheduleErr::BadTime { .. })
        ));
        assert!(matches!(
            parse_schedule(
                "m",
                &entry(Some("07:00"), Some("funday"), None, None, false)
            ),
            Err(ScheduleErr::BadDay { .. })
        ));
        assert!(matches!(
            parse_schedule("m", &entry(None, None, None, Some("0 7 * *"), false)),
            Err(ScheduleErr::BadCron { .. })
        ));
        assert!(matches!(
            parse_schedule("m", &entry(None, None, Some("0m"), None, false)),
            Err(ScheduleErr::BadInterval { .. })
        ));
        assert!(matches!(
            parse_schedule("m", &entry(None, None, Some("later"), None, false)),
            Err(ScheduleErr::BadInterval { .. })
        ));
    }

    #[test]
    fn one_shot_describe_is_prefixed() {
        let parsed =
            parse_schedule("m", &entry(Some("07:00"), None, None, None, true)).expect("parse");
        assert_eq!(parsed.describe(), "once 07:00 every day");
    }

    #[test]
    fn names_are_validated() {
        validate_name("morning").expect("ok");
        validate_name("morning-claude_1").expect("ok");
        assert!(validate_name("").is_err());
        assert!(validate_name("bad name").is_err());
        assert!(validate_name("bad/name").is_err());
    }

    #[test]
    fn run_command_wraps_a_login_shell() {
        let cmd = run_command(Path::new("/usr/local/bin/rimz"), "/bin/zsh", "morning");
        assert_eq!(
            cmd,
            "/bin/zsh -lc 'exec '\\''/usr/local/bin/rimz'\\'' loop run morning'"
        );
    }

    #[test]
    fn crontab_splice_is_idempotent_and_preserves_foreign_lines() {
        let existing = "# my own job\n0 0 * * * backup.sh\n";
        let line = "0 7 * * 1-5 /bin/sh -lc 'rimz loop run morning'";
        let once = splice_crontab(existing, "morning", line);
        let twice = splice_crontab(&once, "morning", line);
        assert_eq!(once, twice, "re-splicing the same name leaves one block");
        assert!(once.contains("# my own job"));
        assert!(once.contains("0 0 * * * backup.sh"));
        assert_eq!(
            list_crontab(&once),
            vec![CronEntry {
                name: "morning".to_owned(),
                line: line.to_owned(),
            }]
        );
    }

    #[test]
    fn crontab_reclaim_removes_only_rimz_blocks() {
        let base = "0 0 * * * backup.sh\n";
        let with_two = splice_crontab(&splice_crontab(base, "a", "LA"), "b", "LB");
        assert_eq!(list_crontab(&with_two).len(), 2);

        let without_a = reclaim_crontab(&with_two, Some("a"));
        let names: Vec<String> = list_crontab(&without_a)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["b".to_owned()]);
        assert!(without_a.contains("0 0 * * * backup.sh"));

        let cleared = reclaim_crontab(&with_two, None);
        assert!(list_crontab(&cleared).is_empty());
        assert_eq!(cleared, "0 0 * * * backup.sh\n");
    }
}
