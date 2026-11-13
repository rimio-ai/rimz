//! Scheduled loop task core.
//!
//! The room host keeps time while a room is open and an opt-in OS
//! timer runs the same scheduler for roots without one. Both fire
//! `rimz loop run <name>`, which drives one configured loop wake-up.
//! Persisted rows compile once into independent action and timing results; a
//! malformed half remains observable without hiding the valid half.
//!
//! This module owns task action validation, catalog precedence and mutation,
//! the [`runner::TaskFire`] state machine, terminal transitions, schedule
//! parsing, and due evaluation. CLI handlers translate flags, execute prepared
//! effects, orchestrate terminal actions, and render.

use std::time::Duration;

use anyhow::Context;

use crate::config::{TaskEntry, TaskTarget};
use crate::harness::prose::{span, verbatim};
use crate::harness::when;
use crate::utils::time::{ClockTime, DurationUnit, parse_duration_units};
use jiff::civil::Weekday;
use jiff::{SignedDuration, Timestamp, Zoned};

pub mod arm;
pub mod arming;
pub mod attention;
pub mod backlog;
mod backlog_store;
pub mod catalog;
pub mod compile;
mod config_edit;
mod eject;
pub mod fanout;
#[doc(hidden)]
pub mod fire;
mod fires;
pub mod instances;
pub mod launch_ledger;
mod launch_ledger_store;
mod overlay_store;
pub mod pending;
pub(crate) mod probe;
mod run_lock;
pub mod run_log;
pub mod runner;
pub mod signal;
pub mod strikes;
pub mod team;

pub use fire::last_stamps;

/// The recorded provenance of a session delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryOwner<'a> {
    SelfWait,
    Loop(&'a str),
    Resident(&'a str),
    Team(&'a crate::ids::TeamInstanceId),
}

impl std::fmt::Display for DeliveryOwner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfWait => f.write_str("self"),
            Self::Loop(name) | Self::Resident(name) => write!(f, "loop:{name}"),
            Self::Team(team) => write!(f, "team:{team}"),
        }
    }
}

/// Classify a catalog delivery, including a plain loop's own name.
pub fn delivery_owner<'a>(name: &'a str, entry: &'a TaskEntry) -> DeliveryOwner<'a> {
    if let Some(team) = &entry.team {
        return DeliveryOwner::Team(team);
    }
    if let Some(task) = entry.loop_task.as_deref() {
        return DeliveryOwner::Resident(task);
    }
    if entry.wait_meta.is_some() {
        return DeliveryOwner::SelfWait;
    }
    DeliveryOwner::Loop(name)
}

/// Whether a missing watch has passed the host's startup grace.
pub fn watch_lost(entry: &TaskEntry, watcher_live: bool, now: Timestamp) -> bool {
    entry.watch.is_some()
        && !watcher_live
        && entry.wait_meta.as_ref().is_none_or(|meta| {
            now.duration_since(meta.armed_at).as_secs() > fire::WATCH_LOST_GRACE_SECS
        })
}

/// Executable action encoded by one loop task entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskAction {
    Spawn(String),
    Workflow(String),
    Deliver(TaskTarget),
    CheckOnly,
}

/// Semantic action shape encoded by one loop task entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskActionKind {
    Spawn,
    Workflow,
    Deliver,
    CheckOnly,
}

impl TaskActionKind {
    pub const fn has_effect(self) -> bool {
        matches!(self, Self::Spawn | Self::Deliver | Self::Workflow)
    }

    pub const fn is_spawn(self) -> bool {
        matches!(self, Self::Spawn)
    }

    pub const fn is_check_only(self) -> bool {
        matches!(self, Self::CheckOnly)
    }

    /// What a fire of this kind does to `subject`, with the row's guard and account, and
    /// ` (you)` when the reader is the delivery's target.
    pub fn text(self, subject: &str, entry: Option<&TaskEntry>, you: bool) -> String {
        let mut action = match self {
            Self::Deliver => subject.split('#').next().unwrap_or(subject).to_owned(),
            Self::Spawn => format!("start {subject}"),
            Self::Workflow => format!("start workflow {subject}"),
            Self::CheckOnly => "run check".into(),
        };
        if let Some(entry) = entry {
            let guard = entry.guard();
            let stages = guard
                .shell
                .map(|_| "check".to_owned())
                .into_iter()
                .chain(
                    guard
                        .judges
                        .iter()
                        .map(|judge| format!("judge `{}`", judge.agent)),
                )
                .collect::<Vec<_>>();
            if !stages.is_empty() && self.has_effect() {
                let guard = stages.join(", then ");
                action = if self.is_spawn() {
                    format!("{guard}, then start {subject}")
                } else {
                    format!("{action} · {guard}")
                };
            }
        }
        if you {
            action.push_str(" (you)");
        }
        if let Some(account) = entry.and_then(|entry| entry.account.as_ref()) {
            action.push_str(&format!(" · account {account}"));
        }
        action
    }
}

/// Invalid combinations of loop task action fields.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskActionErr {
    #[error("loop task `{name}` sets `{field}`; {hint}")]
    OverlapField {
        name: String,
        field: &'static str,
        hint: &'static str,
    },
    #[error(
        "loop task `{name}` sets `verify` without `agent`; verification needs a supervised agent run"
    )]
    VerifyWithoutAgent { name: String },
    #[error(
        "loop task `{name}` sets `account` without `agent`; an account pin needs a supervised agent run"
    )]
    AccountWithoutAgent { name: String },
    #[error("loop task `{name}` sets `max-attempts` without `verify`")]
    AttemptsWithoutVerify { name: String },
    #[error("loop task `{name}` sets `max-attempts` to 0; use at least 1")]
    ZeroAttempts { name: String },
    #[error("loop task `{name}` sets both `agent` and `wait`; keep exactly one")]
    ConflictingActions { name: String },
    #[error("loop task `{name}` sets `workflow` with `agent` or `wait`; keep exactly one")]
    ConflictingWorkflow { name: String },
    #[error("loop task `{name}` sets `{field}` with `workflow`; {hint}")]
    WorkflowField {
        name: String,
        field: &'static str,
        hint: &'static str,
    },
    #[error("loop task `{name}` sets `fresh-worktree` with `{field}`; {hint}")]
    FreshWorktreeField {
        name: String,
        field: &'static str,
        hint: &'static str,
    },
    #[error(
        "loop task `{name}` sets `fresh-worktree` to `{prefix}`, which is not a worktree name; use letters, digits, `-` and `_`, with `/` between segments"
    )]
    InvalidFreshWorktree { name: String, prefix: String },
    #[error("loop task `{name}` needs `agent`, `wait`, or `check`")]
    MissingAction { name: String },
    #[error(
        "loop task `{name}` needs `on` for its judge: `fail` acts on a fail verdict, `success` acts on a pass verdict, `any` acts whatever the verdict"
    )]
    JudgeWithoutOn { name: String },
    #[error(
        "loop task `{name}` needs `agent` or `wait` for its judge; a check-only task runs one shell command"
    )]
    JudgeWithoutAction { name: String },
}

impl TaskAction {
    fn from_entry(name: &str, entry: &TaskEntry) -> Result<Self, TaskActionErr> {
        if entry.overlap_limit.is_some() {
            let hint = if entry.overlap != Some(crate::config::OverlapPolicy::Queue) {
                Some("overlap-limit requires overlap = 'queue'")
            } else if entry.overlap_limit == Some(0) {
                Some("overlap-limit must be at least 1")
            } else {
                None
            };
            if let Some(hint) = hint {
                return Err(TaskActionErr::OverlapField {
                    name: name.to_owned(),
                    field: "overlap-limit",
                    hint,
                });
            }
        }
        if entry.overlap.is_some() {
            for (set, hint) in [
                (
                    entry.wait.is_some(),
                    "a wait delivery has no run to overlap",
                ),
                (
                    entry.each_worktree,
                    "waiting triggers are kept per task, and an each-worktree task fires per checkout",
                ),
                (
                    ephemeral_lifetime(entry),
                    "a one-shot has no task left to drain",
                ),
            ] {
                if set {
                    return Err(TaskActionErr::OverlapField {
                        name: name.to_owned(),
                        field: "overlap",
                        hint,
                    });
                }
            }
        }
        if let Some(prefix) = &entry.fresh_worktree {
            if crate::worktree::parse_requested_name(prefix).is_err() {
                return Err(TaskActionErr::InvalidFreshWorktree {
                    name: name.to_owned(),
                    prefix: prefix.clone(),
                });
            }
            Self::refuse_fresh_worktree_companions(name, entry)?;
        }
        let guard = entry.guard();
        if entry.workflow.is_some() {
            if entry.agent.is_some() || entry.wait.is_some() {
                return Err(TaskActionErr::ConflictingWorkflow {
                    name: name.to_owned(),
                });
            }
            for (set, field, hint) in [
                (
                    entry.throttle.is_some(),
                    "throttle",
                    "capacity policy belongs to the workflow's agent nodes (`agent.<name>.admission.throttle`)",
                ),
                (
                    entry.capacity_wait.is_some(),
                    "capacity-wait",
                    "capacity policy belongs to the workflow's agent nodes (`agent.<name>.admission.give-up`)",
                ),
                (
                    entry.stay,
                    "stay",
                    "a workflow run is not a resident agent; its agent steps launch and stop agents",
                ),
                (
                    entry.takeover,
                    "takeover",
                    "takeover belongs to the workflow's agent nodes (`[agent.<name>.admission] occupancy = \"takeover\"`)",
                ),
                (
                    !entry.subscribe.is_empty(),
                    "subscribe",
                    "signals wake the run's checks; a check names what it waits for",
                ),
                (
                    entry.check.is_some(),
                    "check",
                    "checks are the workflow's `[test.<name>]`, `[script.<name>]`, `[judge.<name>]` and `[condition.<name>]` nodes",
                ),
                (
                    !entry.judge.is_empty(),
                    "judge",
                    "checks are the workflow's `[test.<name>]`, `[script.<name>]`, `[judge.<name>]` and `[condition.<name>]` nodes",
                ),
            ] {
                if set {
                    return Err(TaskActionErr::WorkflowField {
                        name: name.to_owned(),
                        field,
                        hint,
                    });
                }
            }
        }
        if !guard.judges.is_empty() {
            if entry.on.is_none() {
                return Err(TaskActionErr::JudgeWithoutOn {
                    name: name.to_owned(),
                });
            }
            if entry.agent.is_none() && entry.wait.is_none() {
                return Err(TaskActionErr::JudgeWithoutAction {
                    name: name.to_owned(),
                });
            }
        }
        if entry.verify.is_some() && entry.agent.is_none() {
            return Err(TaskActionErr::VerifyWithoutAgent {
                name: name.to_owned(),
            });
        }
        if entry.account.is_some() && entry.agent.is_none() {
            return Err(TaskActionErr::AccountWithoutAgent {
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
        if let Some(workflow) = &entry.workflow {
            return Ok(Self::Workflow(workflow.clone()));
        }
        match (entry.agent.as_deref(), entry.wait.as_ref()) {
            (Some(agent), None) if !agent.trim().is_empty() => Ok(Self::Spawn(agent.to_owned())),
            (None, Some(target)) => Ok(Self::Deliver(target.clone())),
            (None, None) if guard.shell.is_some() => Ok(Self::CheckOnly),
            (Some(_), Some(_)) => Err(TaskActionErr::ConflictingActions {
                name: name.to_owned(),
            }),
            _ => Err(TaskActionErr::MissingAction {
                name: name.to_owned(),
            }),
        }
    }

    fn refuse_fresh_worktree_companions(
        name: &str,
        entry: &TaskEntry,
    ) -> Result<(), TaskActionErr> {
        for (set, field, hint) in [
            (
                entry.stay,
                "stay",
                "remove `fresh-worktree`; a resident's worktree is `worktree`",
            ),
            (
                entry.each_worktree,
                "each-worktree",
                "remove `fresh-worktree`; a fan-out runs in each matching owned worktree",
            ),
            (
                entry.wait.is_some(),
                "wait",
                "remove `fresh-worktree`; a wait delivers to a live session and launches nothing",
            ),
            (
                entry.workflow.is_some(),
                "workflow",
                "remove `fresh-worktree`; a workflow's steps place their own agents",
            ),
            (
                entry.worktree.is_some(),
                "worktree",
                "keep one; `fresh-worktree` creates a new tree each fire, `worktree` reuses one",
            ),
            (
                entry.dir.is_some(),
                "dir",
                "remove `dir`; a fresh-worktree task runs its guard and turn in the fire's tree",
            ),
        ] {
            if set {
                return Err(TaskActionErr::FreshWorktreeField {
                    name: name.to_owned(),
                    field,
                    hint,
                });
            }
        }
        Ok(())
    }

    pub fn subject(&self) -> &str {
        match self {
            Self::Spawn(spec) | Self::Workflow(spec) => spec,
            Self::Deliver(target) => &target.handle,
            Self::CheckOnly => "check",
        }
    }

    pub const fn kind(&self) -> TaskActionKind {
        match self {
            Self::Spawn(_) => TaskActionKind::Spawn,
            Self::Workflow(_) => TaskActionKind::Workflow,
            Self::Deliver(_) => TaskActionKind::Deliver,
            Self::CheckOnly => TaskActionKind::CheckOnly,
        }
    }
}

/// Errors from parsing or validating a schedule entry.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
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
        "schedule `{name}` sets conflicting trigger fields; use one of `when`, `signal`, `watch`, `cron`, `every`, or bare `at`"
    )]
    TriggerConflict { name: String },
    #[error("schedule `{name}` sets `match` without `signal`")]
    MatchWithoutSignal { name: String },
    #[error(transparent)]
    Selector(#[from] crate::harness::signal::SelectorErr),
    #[error("schedule `{name}` sets `once` without `signal` or `when`")]
    OnceWithoutSignal { name: String },
    #[error("schedule `{name}` sets `for` without `when`")]
    ForWithoutWhen { name: String },
    #[error(
        "schedule `{name}` has invalid `for` value `{value}`; use a positive duration in s, m, h, or d"
    )]
    BadFor { name: String, value: String },
    #[error("schedule `{name}`: {source}")]
    BadWhen {
        name: String,
        source: Box<when::WhenError>,
    },
    #[error(
        "schedule `{name}` reads a provider window without `provider`; set `provider = \"<agent kind>\"` or re-add the task with `rimz loop add`"
    )]
    WindowWithoutProvider { name: String },
    #[error("schedule `{name}` sets both `watch` and `check`; the watched command is the check")]
    WatchWithCheck { name: String },
    #[error("schedule `{name}` has an empty watched command, check, file, or pattern")]
    BadWatch { name: String },
    #[error(
        "schedule `{name}` has an invalid check watch: `every` must be a positive duration like `1s` and `on` must be `success` or `fail`"
    )]
    BadCheckWatch { name: String },
    #[error("schedule `{name}` sets a calendar `every` value without `at`; add `at = \"HH:MM\"`")]
    EveryNeedsAt { name: String },
    #[error("schedule `{name}` has an invalid time `{value}`; use 24-hour `HH:MM`")]
    BadTime { name: String, value: String },
    #[error(
        "schedule `{name}` has an invalid one-shot time `{value}`; use `HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`"
    )]
    BadOnceAt { name: String, value: String },
    #[error("schedule `{name}` has a one-shot time `{value}` in the past")]
    PastOnceAt { name: String, value: String },
    #[error("schedule `{name}` has an invalid cron expression `{value}`; expected 5 fields")]
    BadCron { name: String, value: String },
    #[error(
        "schedule `{name}` has an invalid cron expression `{value}`: unknown {field} `{token}`"
    )]
    BadCronToken {
        name: String,
        value: String,
        field: &'static str,
        token: String,
    },
    #[error(
        "schedule `{name}` has a cron expression `{value}` using `{form}`, which is not supported"
    )]
    UnsupportedCron {
        name: String,
        value: String,
        form: String,
    },
    #[error(
        "schedule `{name}` has a cron expression `{value}` that never fires: no selected month has the selected day of month"
    )]
    CronNeverFires { name: String, value: String },
    #[error(
        "schedule `{name}` has an invalid `every` value `{value}`; use a duration like `30m` or a day mask like `weekday` or `mon,wed,fri`"
    )]
    BadEvery { name: String, value: String },
    #[error(transparent)]
    BadName(#[from] crate::harness::name::NameErr),
}

/// Runtime shape compiled once from one persisted task row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskShape {
    action: Result<TaskAction, TaskActionErr>,
    compiled: Result<compile::CompiledAction, TaskActionErr>,
    trigger: Result<ParsedTrigger, ScheduleErr>,
    ephemeral: bool,
    kind: TaskKind,
}

/// The lifecycle of a stored task, independent of its current attention state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Routine,
    OneOff,
    WakeUp,
}

impl TaskShape {
    pub fn compile(name: &str, entry: &TaskEntry) -> Self {
        let action = TaskAction::from_entry(name, entry);
        Self {
            compiled: compile::compile(name, entry, action.as_ref()),
            action,
            trigger: parse_trigger(name, entry),
            ephemeral: ephemeral_lifetime(entry),
            kind: lifecycle_kind(entry),
        }
    }

    pub fn action(&self) -> Result<&TaskAction, &TaskActionErr> {
        self.action.as_ref()
    }

    pub fn compiled(&self) -> Result<&compile::CompiledAction, &TaskActionErr> {
        self.compiled.as_ref()
    }

    pub fn trigger(&self) -> &Result<ParsedTrigger, ScheduleErr> {
        &self.trigger
    }

    const fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }
}

fn lifecycle_kind(entry: &TaskEntry) -> TaskKind {
    if entry.wait.is_some() || entry.team.is_some() || entry.loop_task.is_some() {
        return TaskKind::WakeUp;
    }
    if (entry.signal.is_none()
        && entry.when.is_none()
        && entry.every.is_none()
        && entry.cron.is_none()
        && entry.watch.is_none())
        || entry.fire_at.is_some()
        || entry.once == Some(true)
        || entry.watch.is_some()
        || (entry.stay && !entry.each_worktree)
    {
        return TaskKind::OneOff;
    }
    TaskKind::Routine
}

fn ephemeral_lifetime(entry: &TaskEntry) -> bool {
    (entry.signal.is_none()
        && entry.when.is_none()
        && entry.every.is_none()
        && entry.cron.is_none())
        || entry.deadline.is_some()
        || entry.once == Some(true)
        || entry.watch.is_some()
        || entry.fire_at.is_some()
}

const WEEK: [Weekday; 7] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
    Weekday::Sunday,
];

fn short_day_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

fn parse_weekday(token: &str) -> Option<Weekday> {
    match token.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Some(Weekday::Monday),
        "tue" | "tues" | "tuesday" => Some(Weekday::Tuesday),
        "wed" | "weds" | "wednesday" => Some(Weekday::Wednesday),
        "thu" | "thur" | "thurs" | "thursday" => Some(Weekday::Thursday),
        "fri" | "friday" => Some(Weekday::Friday),
        "sat" | "saturday" => Some(Weekday::Saturday),
        "sun" | "sunday" => Some(Weekday::Sunday),
        _ => None,
    }
}

/// A normalized firing time: minute, hour (local wall-clock), and the weekday
/// set. An empty weekday set means every day.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CalendarSpec {
    pub minute: u8,
    pub hour: u8,
    pub weekdays: Vec<Weekday>,
}

/// A normalized interval schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
struct IntervalSpec {
    pub minutes: u32,
}

impl IntervalSpec {
    fn new(minutes: u32) -> Self {
        Self { minutes }
    }
}

/// `at` to the minute in the machine's time zone, as listings and wakes print an instant.
fn local_minute(at: Timestamp) -> String {
    at.to_zoned(crate::config::MachineConfig::load_lenient().time_zone())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// A parsed schedule: calendar time, interval, a raw cron escape hatch, or
/// the absolute instant of a `fire-at` row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Schedule {
    Calendar(CalendarSpec),
    Interval(IntervalSpec),
    Cron(CronSpec),
    Instant(Timestamp),
}

impl Schedule {
    /// A short human description for listings.
    fn describe(&self) -> String {
        match self {
            Schedule::Cron(cron) => format!("cron `{}`", cron.source),
            Schedule::Calendar(spec) => {
                let days = if spec.weekdays.is_empty() {
                    "day".to_owned()
                } else {
                    spec.weekdays
                        .iter()
                        .map(|day| short_day_name(*day))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                format!("every {days} at {:02}:{:02}", spec.hour, spec.minute)
            }
            Schedule::Interval(spec) => format!("every {}", format_minutes(spec.minutes)),
            Schedule::Instant(at) => format!("once at {}", local_minute(*at)),
        }
    }

    /// Whether this schedule is due now, given the last time its task was
    /// armed or fired. First-sight arming is owned by the elder firing module.
    fn due(&self, last_fire: Timestamp, now: &Zoned) -> bool {
        match self {
            Schedule::Interval(spec) => {
                now.timestamp().duration_since(last_fire).as_secs() >= i64::from(spec.minutes) * 60
            }
            Schedule::Calendar(spec) => calendar_due(spec, last_fire, now),
            Schedule::Cron(cron) => cron.due(last_fire, now),
            Schedule::Instant(at) => last_fire < *at && *at <= now.timestamp(),
        }
    }

    /// First occurrence after `last_fire`, evaluated from `now` in the
    /// configured local zone. A returned timestamp may be at or before `now`,
    /// which means the elder should fire on its next tick.
    fn next_after(&self, last_fire: Timestamp, now: &Zoned) -> Option<Timestamp> {
        match self {
            Schedule::Interval(spec) => last_fire
                .checked_add(SignedDuration::from_secs(i64::from(spec.minutes) * 60))
                .ok(),
            Schedule::Calendar(spec) => calendar_next_after(spec, last_fire, now),
            Schedule::Cron(cron) => cron.next_after(last_fire, now),
            Schedule::Instant(at) => (last_fire < *at).then_some(*at),
        }
    }

    /// The instant of an absolute one-shot, which fires past due on first
    /// sight and ignores the pause and enable edges.
    const fn instant(&self) -> Option<Timestamp> {
        match self {
            Schedule::Instant(at) => Some(*at),
            _ => None,
        }
    }
}

/// A parsed task schedule plus its one-shot flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSchedule {
    schedule: Schedule,
    pub once: bool,
}

/// The event which makes a task eligible to fire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    Schedule(ParsedSchedule),
    Signal {
        selector: crate::harness::signal::SignalSelector,
        matches: std::collections::BTreeMap<String, String>,
    },
    Watch(crate::config::WatchSpec),
    Condition {
        expr: when::WhenExpr,
        hold: Option<Duration>,
    },
}

impl Trigger {
    fn describe(&self) -> String {
        match self {
            Self::Schedule(schedule) => schedule.describe(),
            Self::Signal { selector, matches } => {
                let filters = matches
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join(",");
                if filters.is_empty() {
                    format!("on {selector}")
                } else {
                    format!("on {selector} [{filters}]")
                }
            }
            Self::Watch(spec) => spec.describe(),
            Self::Condition { expr, hold } => {
                let mut description = format!("when {expr}");
                if let Some(hold) = hold {
                    description.push_str(&format!(", for {}", arm::duration_label(*hold)));
                }
                description
            }
        }
    }

    fn resolve(
        &self,
        task_name: &str,
        signal: &crate::harness::signal::Signal,
    ) -> crate::harness::signal::SignalResolution {
        use crate::harness::signal::SignalResolution::{Deliver, Ignore};
        match self {
            Self::Schedule(_) | Self::Condition { .. } => Ignore,
            Self::Signal { selector, matches } => {
                crate::harness::signal::resolve_subscription(selector, matches, signal)
            }
            Self::Watch { .. } if signal.name.as_str() == format!("wait.{task_name}") => Deliver,
            Self::Watch { .. } => Ignore,
        }
    }
}

/// A parsed task trigger plus its one-shot flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTrigger {
    pub trigger: Trigger,
    pub once: bool,
}

impl ParsedTrigger {
    pub fn next_after(&self, now: &Zoned) -> Option<Timestamp> {
        let Trigger::Schedule(parsed) = &self.trigger else {
            return None;
        };
        parsed.schedule.next_after(now.timestamp(), now)
    }

    pub fn describe(&self) -> String {
        self.trigger.describe()
    }
}

/// The task's trigger as its run's brief words it (``on the cron schedule `0 * * * *` ``); a
/// watch, or a trigger that does not parse, has none.
fn trigger_clause(task: &catalog::LoadedTask) -> Option<String> {
    let parsed = task.trigger().as_ref().ok()?;
    let condition_predicate = if task.entry().each_worktree {
        "held in this worktree"
    } else {
        "held"
    };
    Some(match &parsed.trigger {
        // `describe` prints a raw cron as a noun phrase, which no verb can follow.
        Trigger::Schedule(ParsedSchedule {
            schedule: Schedule::Cron(cron),
            ..
        }) => format!("on the cron schedule {}", span(&cron.source)),
        Trigger::Schedule(_) => verbatim(&parsed.describe()),
        Trigger::Signal { .. } => {
            let described = parsed.describe();
            let selector = described.strip_prefix("on ").unwrap_or(&described);
            format!("on the signal {}", span(selector))
        }
        Trigger::Condition { expr, hold } => {
            let mut clause = format!("when {} {condition_predicate}", span(&expr.to_string()));
            if let Some(hold) = hold {
                clause.push_str(&format!(" for {}", arm::duration_label(*hold)));
            }
            clause
        }
        Trigger::Watch(_) => return None,
    })
}

impl ParsedSchedule {
    /// A short human description for listings.
    fn describe(&self) -> String {
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
    parsed: Result<ParsedTrigger, ScheduleErr>,
    state: TaskTimingState,
    arm_state: arming::ArmState,
    scheduled_next: Option<Timestamp>,
}

/// Current schedule state before CLI presentation and live run-lock overlays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskTimingState {
    Blocked(crate::trust::TrustState),
    Disabled(arming::DisabledReason),
    Paused(Timestamp),
    Invalid,
    Unarmed,
    Upcoming(Timestamp),
    Due(Timestamp),
    NoOccurrence,
    Listening {
        name: crate::harness::signal::SignalSelector,
    },
    Watching {
        spec: crate::config::WatchSpec,
    },
    /// The condition reads false. An unread key reads `None`.
    Waiting {
        readings: Vec<(String, Option<String>)>,
    },
    /// The condition is unknown: an unread key keeps it from true or false.
    Unknown {
        readings: Vec<(String, Option<String>)>,
    },
    Holding {
        elapsed: Duration,
        hold: Duration,
    },
    Fired,
}

impl TaskTimingState {
    /// The state of a condition that is not true now: `Waiting` on false, `Unknown` on unknown.
    pub fn unmet(expr: &when::WhenExpr, verdict: &when::Verdict) -> Option<Self> {
        let readings = expr
            .readings(verdict)
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect();
        match verdict.truth {
            Some(true) => None,
            Some(false) => Some(Self::Waiting { readings }),
            None => Some(Self::Unknown { readings }),
        }
    }

    pub fn condition_label(&self) -> Option<String> {
        let readings = |readings: &[(String, Option<String>)]| {
            readings
                .iter()
                .map(|(key, value)| format!("{key}: {}", value.as_deref().unwrap_or("unknown")))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Self::Waiting { readings: values } => Some(format!("waiting · {}", readings(values))),
            Self::Unknown { readings: values } => Some(format!("unknown · {}", readings(values))),
            Self::Holding { elapsed, hold } => Some(format!(
                "holding {}/{}",
                crate::utils::time::format_duration_coarse(
                    elapsed.as_secs().min(i64::MAX as u64) as i64
                ),
                arm::duration_label(*hold)
            )),
            Self::Fired => Some("fired".to_owned()),
            Self::Due(_) => Some("due".to_owned()),
            _ => None,
        }
    }
}

impl TaskTiming {
    /// Project conjunction progress from the readings already held by this timing.
    pub fn condition_count(&self) -> Option<when::ConditionCount> {
        let Trigger::Condition { expr, .. } = &self.parsed.as_ref().ok()?.trigger else {
            return None;
        };
        let readings = match &self.state {
            TaskTimingState::Waiting { readings } | TaskTimingState::Unknown { readings } => {
                readings.as_slice()
            }
            TaskTimingState::Holding { .. } | TaskTimingState::Fired | TaskTimingState::Due(_) => {
                &[][..]
            }
            _ => return None,
        };
        expr.condition_count(
            readings,
            matches!(
                self.state,
                TaskTimingState::Holding { .. } | TaskTimingState::Fired | TaskTimingState::Due(_)
            ),
        )
    }

    /// Refine an armed condition from the same current reading used by the CLI.
    pub fn with_condition(
        mut self,
        verdict: &when::Verdict,
        state: Option<&when::WhenState>,
        now: Timestamp,
    ) -> Self {
        if !matches!(self.state, TaskTimingState::NoOccurrence) {
            return self;
        }
        let Ok(ParsedTrigger {
            trigger: Trigger::Condition { expr, hold },
            ..
        }) = &self.parsed
        else {
            return self;
        };
        self.state = if let Some(unmet) = TaskTimingState::unmet(expr, verdict) {
            unmet
        } else if state.is_some_and(|state| state.fired) {
            TaskTimingState::Fired
        } else if let Some(hold) = hold {
            TaskTimingState::Holding {
                elapsed: state
                    .map(|state| {
                        Duration::from_millis(
                            now.as_millisecond()
                                .saturating_sub(state.since.as_millisecond())
                                .max(0) as u64,
                        )
                    })
                    .unwrap_or_default(),
                hold: *hold,
            }
        } else {
            TaskTimingState::Due(now)
        };
        self
    }
    pub fn evaluate(
        parsed: &Result<ParsedTrigger, ScheduleErr>,
        source: catalog::TaskSource,
        last_fire: Option<Timestamp>,
        arming: Option<&arming::Arming>,
        now: &Zoned,
    ) -> Self {
        let parsed = parsed.clone();
        let arm_state = arming::ArmState::resolve(arming, source, now.timestamp());
        let scheduled_next = match (&parsed, last_fire) {
            (
                Ok(ParsedTrigger {
                    trigger:
                        Trigger::Schedule(ParsedSchedule {
                            schedule: Schedule::Instant(at),
                            ..
                        }),
                    ..
                }),
                last_fire,
            ) => last_fire
                .is_none_or(|last_fire| last_fire < *at)
                .then_some(*at),
            (
                Ok(ParsedTrigger {
                    trigger: Trigger::Schedule(parsed),
                    ..
                }),
                Some(last_fire),
            ) => parsed.schedule.next_after(
                arming::effective_last_fire(last_fire, arming, now.timestamp()),
                now,
            ),
            (Ok(_), Some(_)) => None,
            (Ok(_), None) | (Err(_), _) => None,
        };
        let state = if let Some(state) = source.blocked_state() {
            TaskTimingState::Blocked(state)
        } else {
            match arm_state {
                arming::ArmState::Disabled(reason) => TaskTimingState::Disabled(reason),
                arming::ArmState::Paused(until) => TaskTimingState::Paused(until),
                arming::ArmState::Live => match (&parsed, last_fire) {
                    (Err(_), _) => TaskTimingState::Invalid,
                    (
                        Ok(ParsedTrigger {
                            trigger: Trigger::Signal { selector, .. },
                            ..
                        }),
                        _,
                    ) => TaskTimingState::Listening {
                        name: selector.clone(),
                    },
                    (
                        Ok(ParsedTrigger {
                            trigger: Trigger::Watch(spec),
                            ..
                        }),
                        _,
                    ) => TaskTimingState::Watching { spec: spec.clone() },
                    (Ok(_), None) if scheduled_next.is_none() => TaskTimingState::Unarmed,
                    (Ok(_), _) => match scheduled_next {
                        Some(next) if next <= now.timestamp() => TaskTimingState::Due(next),
                        Some(next) => TaskTimingState::Upcoming(next),
                        None => TaskTimingState::NoOccurrence,
                    },
                },
            }
        };
        Self {
            parsed,
            state,
            arm_state,
            scheduled_next,
        }
    }

    pub fn state(&self) -> TaskTimingState {
        self.state.clone()
    }

    pub fn parsed(&self) -> Result<&ParsedTrigger, &ScheduleErr> {
        self.parsed.as_ref()
    }

    pub const fn arm_state(&self) -> arming::ArmState {
        self.arm_state
    }

    pub const fn next_timestamp(&self) -> Option<Timestamp> {
        match &self.state {
            TaskTimingState::Upcoming(next) | TaskTimingState::Due(next) => Some(*next),
            _ => None,
        }
    }

    /// Parsed schedule occurrence independent of trust and arming display overlays.
    pub const fn scheduled_next_timestamp(&self) -> Option<Timestamp> {
        self.scheduled_next
    }
}

/// The instant a one-shot `--in` fires: `delay` after `now`, rounded up to the next whole
/// minute, so its `once at` label names the minute the scheduler fires it.
pub fn delayed_instant(delay: Duration, now: Timestamp) -> anyhow::Result<Timestamp> {
    const MINUTE: i128 = 60_000_000_000;
    let target = now
        .checked_add(delay)
        .context("resolving --in against the clock")?
        .as_nanosecond();
    let rounded = target.div_euclid(MINUTE) * MINUTE
        + if target.rem_euclid(MINUTE) == 0 {
            0
        } else {
            MINUTE
        };
    Timestamp::from_nanosecond(rounded).context("rounding --in to the next scheduler minute")
}

/// Resolve a one-shot `--at` (`HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`) to the first
/// matching instant strictly after `now`, in `now`'s zone. A dated time at or before `now` is
/// refused as in the past.
pub fn resolve_once_at(name: &str, raw: &str, now: &Zoned) -> Result<Timestamp, ScheduleErr> {
    let bad = || ScheduleErr::BadOnceAt {
        name: name.to_owned(),
        value: raw.to_owned(),
    };
    let trimmed = raw.trim();
    let (day, time) = match trimmed.split_once(' ') {
        Some((day, time)) => (Some(day), time),
        None => (None, trimmed),
    };
    if time.starts_with(char::is_whitespace) {
        return Err(bad());
    }
    let time = time.parse::<ClockTime>().map_err(|_| bad())?;
    let at = |date: jiff::civil::Date| {
        date.at(time.hour() as i8, time.minute() as i8, 0, 0)
            .to_zoned(now.time_zone().clone())
            .map(|zoned| zoned.timestamp())
            .map_err(|_| bad())
    };
    let Some(day) = day else {
        let today = at(now.date())?;
        if today > now.timestamp() {
            return Ok(today);
        }
        return at(now.date().tomorrow().map_err(|_| bad())?);
    };
    if let Some(weekday) = parse_weekday(day) {
        let mut date = now.date();
        loop {
            if date.weekday() == weekday {
                let candidate = at(date)?;
                if candidate > now.timestamp() {
                    return Ok(candidate);
                }
            }
            date = date.tomorrow().map_err(|_| bad())?;
        }
    }
    let is_iso_date = day.len() == 10
        && day.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    let date: jiff::civil::Date = is_iso_date
        .then(|| day.parse().ok())
        .flatten()
        .ok_or_else(bad)?;
    let candidate = at(date)?;
    if candidate <= now.timestamp() {
        return Err(ScheduleErr::PastOnceAt {
            name: name.to_owned(),
            value: raw.to_owned(),
        });
    }
    Ok(candidate)
}

/// Parse and validate an entry's firing time into a [`ParsedSchedule`]. Full
/// agent preflight is validated separately by the CLI.
fn parse_schedule(name: &str, entry: &TaskEntry) -> Result<ParsedSchedule, ScheduleErr> {
    let schedule = match TimingFields::classify(entry) {
        TimingFields::Conflict => {
            return Err(ScheduleErr::TimeConflict {
                name: name.to_owned(),
            });
        }
        TimingFields::Cron(cron) => Schedule::Cron(CronSpec::parse(name, cron)?),
        TimingFields::Every { every, at } => match parse_every(name, every)? {
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
        TimingFields::Once(at) => {
            let (hour, minute) = parse_task_hhmm(name, at)?;
            Schedule::Calendar(CalendarSpec {
                minute,
                hour,
                weekdays: Vec::new(),
            })
        }
        TimingFields::Instant(at) => Schedule::Instant(at),
        TimingFields::Missing => {
            return Err(ScheduleErr::NoTime {
                name: name.to_owned(),
            });
        }
    };
    Ok(ParsedSchedule {
        schedule,
        once: entry.every.is_none() && entry.cron.is_none(),
    })
}

/// Parse and validate the single trigger encoded by a task row.
pub fn parse_trigger(name: &str, entry: &TaskEntry) -> Result<ParsedTrigger, ScheduleErr> {
    let has_schedule = entry.at.is_some()
        || entry.every.is_some()
        || entry.cron.is_some()
        || entry.fire_at.is_some();
    let has_signal = entry.signal.is_some();
    let has_watch = entry.watch.is_some();
    let has_when = entry.when.is_some();
    if usize::from(has_schedule)
        + usize::from(has_signal)
        + usize::from(has_watch)
        + usize::from(has_when)
        > 1
        || (has_when && entry.deadline.is_some())
    {
        return Err(ScheduleErr::TriggerConflict {
            name: name.to_owned(),
        });
    }
    if entry.matches.is_some() && !has_signal {
        return Err(ScheduleErr::MatchWithoutSignal {
            name: name.to_owned(),
        });
    }
    if entry.hold.is_some() && !has_when {
        return Err(ScheduleErr::ForWithoutWhen {
            name: name.to_owned(),
        });
    }
    if entry.once.is_some() && !has_signal && !has_when {
        return Err(ScheduleErr::OnceWithoutSignal {
            name: name.to_owned(),
        });
    }
    let guard = entry.guard();
    if has_watch && (guard.shell.is_some() || !guard.judges.is_empty()) {
        return Err(ScheduleErr::WatchWithCheck {
            name: name.to_owned(),
        });
    }
    let trigger = if let Some(clauses) = &entry.when {
        let expr = when::WhenExpr::parse(clauses).map_err(|source| ScheduleErr::BadWhen {
            name: name.to_owned(),
            source: Box::new(source),
        })?;
        if entry.provider.is_none() && expr.window_spans().next().is_some() {
            return Err(ScheduleErr::WindowWithoutProvider {
                name: name.to_owned(),
            });
        }
        let hold = entry
            .hold
            .as_deref()
            .map(|value| {
                parse_duration_units(
                    value,
                    &[
                        DurationUnit::Second,
                        DurationUnit::Minute,
                        DurationUnit::Hour,
                        DurationUnit::Day,
                    ],
                )
                .ok()
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| ScheduleErr::BadFor {
                    name: name.to_owned(),
                    value: value.to_owned(),
                })
            })
            .transpose()?;
        Trigger::Condition { expr, hold }
    } else if let Some(raw) = entry.signal.as_deref() {
        let selector =
            crate::harness::signal::parse_signal_selector(name, raw, entry.matches.as_ref())?;
        Trigger::Signal {
            selector,
            matches: entry.matches.clone().unwrap_or_default(),
        }
    } else if let Some(spec) = &entry.watch {
        validate_watch(name, spec)?;
        Trigger::Watch(spec.clone())
    } else {
        Trigger::Schedule(parse_schedule(name, entry)?)
    };
    Ok(ParsedTrigger {
        once: matches!(trigger, Trigger::Watch(_)) || entry.once == Some(true),
        trigger,
    })
}

fn validate_watch(name: &str, spec: &crate::config::WatchSpec) -> Result<(), ScheduleErr> {
    use crate::config::{CheckOn, WatchSpec};
    let name = name.to_owned();
    match spec {
        WatchSpec::Command(command) | WatchSpec::Check { check: command, .. }
            if command.trim().is_empty() =>
        {
            Err(ScheduleErr::BadWatch { name })
        }
        WatchSpec::File { file, grep, .. }
            if file.as_os_str().is_empty() || grep.as_deref().is_some_and(str::is_empty) =>
        {
            Err(ScheduleErr::BadWatch { name })
        }
        WatchSpec::Check {
            on: CheckOn::Any, ..
        } => Err(ScheduleErr::BadCheckWatch { name }),
        WatchSpec::Check { .. } if watch_interval(spec).is_none_or(|every| every.is_zero()) => {
            Err(ScheduleErr::BadCheckWatch { name })
        }
        WatchSpec::Command(_)
        | WatchSpec::Pid { .. }
        | WatchSpec::Check { .. }
        | WatchSpec::File { .. } => Ok(()),
    }
}

/// `rimz wait --every` stays under a day, so its label never needs days.
const WATCH_EVERY_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
];

/// How long a polled watch sleeps between probes; `None` for a run-once
/// command or an unparseable `every`.
fn watch_interval(spec: &crate::config::WatchSpec) -> Option<std::time::Duration> {
    use crate::config::WatchSpec;
    match spec {
        WatchSpec::Command(_) => None,
        WatchSpec::Pid { .. } | WatchSpec::File { .. } => Some(std::time::Duration::from_secs(1)),
        WatchSpec::Check { every, .. } => parse_duration_units(every, WATCH_EVERY_UNITS).ok(),
    }
}

enum TimingFields<'a> {
    Conflict,
    Cron(&'a str),
    Every { every: &'a str, at: Option<&'a str> },
    Once(&'a str),
    Instant(Timestamp),
    Missing,
}

impl<'a> TimingFields<'a> {
    fn classify(entry: &'a TaskEntry) -> Self {
        if let Some(at) = entry.fire_at {
            return if entry.cron.is_some() || entry.every.is_some() || entry.at.is_some() {
                Self::Conflict
            } else {
                Self::Instant(at)
            };
        }
        match (
            entry.cron.as_deref(),
            entry.every.as_deref(),
            entry.at.as_deref(),
        ) {
            (Some(cron), None, None) => Self::Cron(cron),
            (Some(_), _, _) => Self::Conflict,
            (None, Some(every), at) => Self::Every { every, at },
            (None, None, Some(at)) => Self::Once(at),
            (None, None, None) => Self::Missing,
        }
    }
}

enum EverySpec {
    Interval(u32),
    Days(Vec<Weekday>),
}

fn parse_every(name: &str, raw: &str) -> Result<EverySpec, ScheduleErr> {
    let value = raw.trim();
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
    value
        .parse::<ClockTime>()
        .map(|time| (time.hour(), time.minute()))
        .map_err(|_| bad())
}

fn parse_days(days: &str) -> Option<Vec<Weekday>> {
    let days = days.trim();
    if days.is_empty() {
        return None;
    }
    match days.to_ascii_lowercase().as_str() {
        "day" | "daily" => return Some(Vec::new()),
        "weekday" | "weekdays" => return Some(weekday_range(Weekday::Monday, Weekday::Friday)),
        "weekend" | "weekends" => return Some(vec![Weekday::Saturday, Weekday::Sunday]),
        _ => {}
    }
    let mut set: Vec<Weekday> = Vec::new();
    for token in days.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        let expanded = if let Some((lo, hi)) = token.split_once('-') {
            let lo = parse_weekday(lo)?;
            let hi = parse_weekday(hi)?;
            weekday_range(lo, hi)
        } else {
            vec![parse_weekday(token)?]
        };
        for day in expanded {
            if !set.contains(&day) {
                set.push(day);
            }
        }
    }
    set.sort_by_key(|day| day.to_monday_zero_offset());
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
    let (lo, hi) = (
        lo.to_monday_zero_offset() as usize,
        hi.to_monday_zero_offset() as usize,
    );
    if lo <= hi {
        WEEK[lo..=hi].to_vec()
    } else {
        WEEK[lo..].iter().chain(&WEEK[..=hi]).copied().collect()
    }
}

/// A cron expression parsed once: each field's selected values as a bit set, beside the source
/// text listings and stored rows keep.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CronSpec {
    source: String,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    /// Sunday is bit 0, whether written `0` or `7`.
    weekdays: u64,
    /// Whether both day fields are anything but exactly `*`, when cron's OR rule matches a day
    /// either field selects.
    either_day: bool,
}

/// One cron field: its label in a refusal, its range, and the names it takes, the first
/// standing for `min`.
struct CronField {
    label: &'static str,
    min: u8,
    max: u8,
    names: &'static [&'static str],
}

const CRON_MONTH_NAMES: &[&str] = &[
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const CRON_DAY_NAMES: &[&str] = &["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const CRON_FIELDS: [CronField; 5] = [
    CronField {
        label: "minute",
        min: 0,
        max: 59,
        names: &[],
    },
    CronField {
        label: "hour",
        min: 0,
        max: 23,
        names: &[],
    },
    CronField {
        label: "day-of-month",
        min: 1,
        max: 31,
        names: &[],
    },
    CronField {
        label: "month",
        min: 1,
        max: 12,
        names: CRON_MONTH_NAMES,
    },
    CronField {
        label: "day-of-week",
        min: 0,
        max: 7,
        names: CRON_DAY_NAMES,
    },
];
/// Days in each month, February counted with its leap day.
const CRON_MONTH_DAYS: [u8; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
/// Eight years of days reach a 29 February from any date, across the 2100 gap as well.
const CRON_SEARCH_DAYS: usize = 8 * 366;

impl CronSpec {
    fn parse(name: &str, cron: &str) -> Result<Self, ScheduleErr> {
        let unsupported = |form: &str| ScheduleErr::UnsupportedCron {
            name: name.to_owned(),
            value: cron.to_owned(),
            form: form.to_owned(),
        };
        let unknown = |field: &'static str, token: &str| ScheduleErr::BadCronToken {
            name: name.to_owned(),
            value: cron.to_owned(),
            field,
            token: token.to_owned(),
        };
        let source = cron.trim();
        let expanded = if source.starts_with('@') && !source.contains(char::is_whitespace) {
            if source.eq_ignore_ascii_case("@reboot") {
                return Err(unsupported(source));
            }
            cron_macro(source).ok_or_else(|| unknown("expression", source))?
        } else {
            source
        };
        let fields: Vec<&str> = expanded.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else {
            return Err(ScheduleErr::BadCron {
                name: name.to_owned(),
                value: cron.to_owned(),
            });
        };
        let mut sets = [0; 5];
        for ((text, field), set) in [minute, hour, day, month, weekday]
            .into_iter()
            .zip(&CRON_FIELDS)
            .zip(&mut sets)
        {
            *set = parse_cron_field(text, field).map_err(|token| {
                unsupported_cron_form(token)
                    .map_or_else(|| unknown(field.label, token), unsupported)
            })?;
        }
        let [minutes, hours, days, months, weekdays] = sets;
        if *day != "*" && *weekday == "*" && !cron_month_has_day(months, days) {
            return Err(ScheduleErr::CronNeverFires {
                name: name.to_owned(),
                value: cron.to_owned(),
            });
        }
        Ok(Self {
            source: source.to_owned(),
            minutes,
            hours,
            days,
            months,
            weekdays: (weekdays | weekdays >> 7) & 0x7f,
            either_day: *day != "*" && *weekday != "*",
        })
    }

    fn matches_day(&self, date: jiff::civil::Date) -> bool {
        let day = self.days & 1 << date.day() != 0;
        let weekday = self.weekdays & 1 << date.weekday().to_sunday_zero_offset() != 0;
        let day_matches = if self.either_day {
            day || weekday
        } else {
            day && weekday
        };
        day_matches && self.months & 1 << date.month() != 0
    }

    fn matches(&self, now: &Zoned) -> bool {
        self.matches_day(now.date())
            && self.hours & 1 << now.hour() != 0
            && self.minutes & 1 << now.minute() != 0
    }

    fn due(&self, last_fire: Timestamp, now: &Zoned) -> bool {
        self.matches(now) && minute_bucket(last_fire) < minute_bucket(now.timestamp())
    }

    /// The first selected minute after `now` (or `now` itself while it is due), found day by
    /// day and then minute by minute within the first day that holds one.
    fn next_after(&self, last_fire: Timestamp, now: &Zoned) -> Option<Timestamp> {
        if self.due(last_fire, now) {
            return Some(now.timestamp());
        }
        let zone = now.time_zone();
        let from = (minute_bucket(now.timestamp()) + 1) * 60;
        let mut date = now.date();
        for _ in 0..CRON_SEARCH_DAYS {
            let next = date.tomorrow().ok()?;
            if self.matches_day(date) {
                let start = date.to_zoned(zone.clone()).ok()?.timestamp().as_second();
                let end = next.to_zoned(zone.clone()).ok()?.timestamp().as_second();
                let found = (start.max(from)..end).step_by(60).find_map(|second| {
                    let at = Timestamp::from_second(second).ok()?;
                    self.matches(&at.to_zoned(zone.clone())).then_some(at)
                });
                if found.is_some() {
                    return found;
                }
            }
            date = next;
        }
        None
    }
}

fn cron_macro(source: &str) -> Option<&'static str> {
    Some(match source.to_ascii_lowercase().as_str() {
        "@hourly" => "0 * * * *",
        "@daily" => "0 0 * * *",
        "@weekly" => "0 0 * * 0",
        "@monthly" => "0 0 1 * *",
        "@yearly" => "0 0 1 1 *",
        _ => return None,
    })
}

/// The values a field's comma list selects, or the item it cannot read.
fn parse_cron_field<'a>(text: &'a str, field: &CronField) -> Result<u64, &'a str> {
    text.split(',').try_fold(0, |set, item| {
        parse_cron_item(item, field)
            .map(|values| set | values)
            .ok_or(item)
    })
}

/// One item: `*`, a value, or a range, a range or `*` taking a `/n` step.
fn parse_cron_item(item: &str, field: &CronField) -> Option<u64> {
    let (range, step) = match item.split_once('/') {
        Some((range, step)) => (
            range,
            Some(step.parse::<u8>().ok().filter(|step| *step > 0)?),
        ),
        None => (item, None),
    };
    let (lo, hi) = if range == "*" {
        (field.min, field.max)
    } else if let Some((lo, hi)) = range.split_once('-') {
        (cron_value(lo, field)?, cron_value(hi, field)?)
    } else if step.is_none() {
        let value = cron_value(range, field)?;
        (value, value)
    } else {
        return None;
    };
    (lo <= hi).then(|| {
        (lo..=hi)
            .step_by(step.unwrap_or(1).into())
            .fold(0, |set, value| set | 1 << value)
    })
}

fn cron_value(text: &str, field: &CronField) -> Option<u8> {
    let value = match field
        .names
        .iter()
        .position(|name| name.eq_ignore_ascii_case(text))
    {
        Some(index) => field.min + u8::try_from(index).ok()?,
        None => text.parse().ok()?,
    };
    (field.min..=field.max).contains(&value).then_some(value)
}

/// The unsupported form a refused item carries (`?`, `L`, `W`), read past day and month names.
/// A word of letters alone (`wed` in the wrong field, `wednesday`) stays an unknown token
/// unless it is exactly `L`, `W` or `LW`.
fn unsupported_cron_form(item: &str) -> Option<&'static str> {
    if item.contains('?') {
        return Some("?");
    }
    let is_word = item.chars().all(|c| c.is_ascii_alphabetic());
    if is_word
        && !["l", "w", "lw"]
            .iter()
            .any(|form| form.eq_ignore_ascii_case(item))
    {
        return None;
    }
    item.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|word| {
            !CRON_DAY_NAMES
                .iter()
                .chain(CRON_MONTH_NAMES)
                .any(|name| name.eq_ignore_ascii_case(word))
        })
        .flat_map(str::chars)
        .find_map(|c| match c.to_ascii_uppercase() {
            'L' => Some("L"),
            'W' => Some("W"),
            _ => None,
        })
}

/// Whether some selected month has a selected day of month, February counting 29 days.
fn cron_month_has_day(months: u64, days: u64) -> bool {
    (1..=12u8)
        .filter(|month| months & 1 << month != 0)
        .any(|month| {
            let length = CRON_MONTH_DAYS[usize::from(month - 1)];
            days & ((1u64 << (length + 1)) - 2) != 0
        })
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

fn calendar_due(spec: &CalendarSpec, last_fire: Timestamp, now: &Zoned) -> bool {
    if !spec.weekdays.is_empty() && !spec.weekdays.contains(&now.weekday()) {
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
        if !spec.weekdays.is_empty() && !spec.weekdays.contains(&date.weekday()) {
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

fn minute_bucket(timestamp: Timestamp) -> i64 {
    timestamp.as_second().div_euclid(60)
}

#[cfg(test)]
pub(super) mod tests;
