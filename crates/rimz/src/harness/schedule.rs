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

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;

use crate::config::{LaunchKind, OverlapPolicy, TaskEntry, TaskTarget};
use crate::harness::prose::{span, verbatim};
use crate::harness::when;
use crate::utils::time::{CalendarAt, DurationUnit, OnceAt, parse_duration_units};
use jiff::{Timestamp, ZonedDateTime};

pub mod arm;
pub mod arming;
pub mod attention;
pub mod backlog;
mod backlog_store;
pub mod catalog;
mod config_edit;
pub mod fanout;
#[doc(hidden)]
pub mod fire;
mod fires;
pub mod instances;
mod launch;
pub mod launches;
pub mod pending;
pub(crate) mod probe;
pub mod run_control;
mod run_lock;
pub mod run_log;
pub mod runner;
pub mod shell_task;
pub mod signal;
pub mod strikes;
pub mod task_record;
mod task_record_store;
pub mod team;
#[cfg(test)]
pub(crate) mod test_homes;
pub mod wait;
pub mod window;

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
    /// The argv a shell fire runs in its checkout.
    Shell(Vec<String>),
    /// A workflow, agent or team launch a fire makes through its own command.
    Launch(TaskLaunch),
    Deliver(TaskTarget),
}

/// A non-shell fire's launch: `rimz workflow run <target>`, `rimz agents <target>` or
/// `rimz teams launch <target>`, each followed by the row's `-w <worktree>` and `args`
/// unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskLaunch {
    pub kind: LaunchKind,
    pub target: String,
    /// The row's `worktree`, handed to the launch as `-w` before `args`.
    pub worktree: Option<String>,
    pub args: Vec<String>,
}

impl TaskLaunch {
    /// The `rimz` argv the launch runs, without the program.
    pub fn argv(&self) -> Vec<String> {
        let head: &[&str] = match self.kind {
            LaunchKind::Workflow => &["workflow", "run"],
            LaunchKind::Agent => &["agents"],
            LaunchKind::Team => &["teams", "launch"],
            // A shell row is `TaskAction::Shell`; `from_entry` never builds a shell launch.
            LaunchKind::Shell => &[],
        };
        head.iter()
            .map(|word| (*word).to_owned())
            .chain([self.target.clone()])
            .chain(
                self.worktree
                    .iter()
                    .flat_map(|name| ["-w".to_owned(), name.clone()]),
            )
            .chain(self.args.iter().cloned())
            .collect()
    }
}

/// Semantic action shape encoded by one loop task entry: the declared kind of a launch row, or a
/// delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskActionKind {
    Shell,
    Workflow,
    Agent,
    Team,
    Deliver,
}

impl TaskActionKind {
    /// The word JSON views name the kind by.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Workflow => "workflow",
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Deliver => "deliver",
        }
    }

    /// Whether a fire of this kind launches something rather than delivering a message.
    pub const fn launches(self) -> bool {
        !matches!(self, Self::Deliver)
    }

    /// What a fire of this kind does to `subject`, with the row's account, and ` (you)` when
    /// the reader is the delivery's target.
    pub fn text(self, subject: &str, entry: Option<&TaskEntry>, you: bool) -> String {
        let mut action = match self {
            Self::Deliver => subject.split('#').next().unwrap_or(subject).to_owned(),
            Self::Shell => format!("run {subject}"),
            Self::Workflow | Self::Agent | Self::Team => format!("launch {subject}"),
        };
        if you {
            action.push_str(" (you)");
        }
        if let Some(account) = entry.and_then(|entry| entry.account.as_ref()) {
            action.push_str(&format!(" · account {account}"));
        }
        action
    }
}

impl From<LaunchKind> for TaskActionKind {
    fn from(kind: LaunchKind) -> Self {
        match kind {
            LaunchKind::Shell => Self::Shell,
            LaunchKind::Workflow => Self::Workflow,
            LaunchKind::Agent => Self::Agent,
            LaunchKind::Team => Self::Team,
        }
    }
}

/// The re-add advice every refused row ends with.
const READD: &str = "delete the task and re-add it with `rimz loop add`";

/// Invalid combinations of loop task action fields.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskActionErr {
    #[error("loop task `{name}` is not a row this RimZ takes ({detail}); {READD}")]
    Refused { name: String, detail: String },
    #[error("loop task `{name}` sets `{field}`; {hint}")]
    OverlapField {
        name: String,
        field: &'static str,
        hint: &'static str,
    },
    #[error("loop task `{name}` sets `{field}` beside `wait`; keep exactly one action")]
    ConflictingActions { name: String, field: &'static str },
    #[error("loop task `{name}` has no `args` and no `kind`; {READD}")]
    MissingAction { name: String },
    #[error(
        "loop task `{name}` is a shell row without `timeout`; set `timeout = \"2h\"`, or {READD}"
    )]
    MissingTimeout { name: String },
    #[error("loop task `{name}` of kind `{kind}` has no `target`; {READD}")]
    MissingTarget { name: String, kind: &'static str },
    #[error("loop task `{name}` of kind `{kind}` sets `{field}`; {hint}")]
    KindField {
        name: String,
        kind: &'static str,
        field: &'static str,
        hint: String,
    },
    #[error(
        "loop task `{name}` sets `each-worktree` on a trigger that fires once, which would remove the task after its first checkout; set `repeat = true` beside `when`, or fire on `cron`"
    )]
    OneShotFanOut { name: String },
}

impl TaskAction {
    fn from_entry(name: &str, entry: &TaskEntry) -> Result<Self, TaskActionErr> {
        let refused = |detail: &str| TaskActionErr::Refused {
            name: name.to_owned(),
            detail: detail.to_owned(),
        };
        if let Some(refused_row) = &entry.refused {
            return Err(refused(&refused_row.detail));
        }
        if entry.on.is_some() && entry.watch.is_none() {
            return Err(refused("`on` belongs to a watch"));
        }
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
        if let Some(policy) = entry.overlap {
            let allow = policy == OverlapPolicy::Allow;
            for (set, hint) in [
                (
                    entry.wait.is_some(),
                    "a wait delivery has no run to overlap",
                ),
                (
                    entry.each_worktree,
                    if allow {
                        "an each-worktree task takes no overlap policy, since it fires per checkout"
                    } else {
                        "waiting triggers are kept per task, and an each-worktree task fires per checkout"
                    },
                ),
                (
                    ephemeral_lifetime(entry),
                    if allow {
                        "a one-shot fires once, so no later fire overlaps it"
                    } else {
                        "a one-shot has no task left to drain"
                    },
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
        if let Some(worktree) = &entry.worktree
            && let Err(error) = crate::worktree::parse_requested_name(worktree)
        {
            return Err(refused(&format!("`worktree`: {error}")));
        }
        if entry.each_worktree && entry.worktree.is_some() {
            return Err(refused(
                "`worktree` places every fire, and `each-worktree` places each fire",
            ));
        }
        if entry.each_worktree && !repeats(entry) {
            return Err(TaskActionErr::OneShotFanOut {
                name: name.to_owned(),
            });
        }
        if entry.wait.is_some() {
            for (set, field) in [
                (entry.kind.is_some(), "kind"),
                (entry.target.is_some(), "target"),
                (!entry.args.is_empty(), "args"),
                (entry.worktree.is_some(), "worktree"),
            ] {
                if set {
                    return Err(TaskActionErr::ConflictingActions {
                        name: name.to_owned(),
                        field,
                    });
                }
            }
        }
        if let Some(target) = &entry.wait {
            return Ok(Self::Deliver(target.clone()));
        }
        if entry.kind.is_none() && entry.args.is_empty() {
            return Err(TaskActionErr::MissingAction {
                name: name.to_owned(),
            });
        }
        if entry.prompt.is_some() {
            return Err(refused("`prompt` belongs to a delivery row"));
        }
        let kind = entry.kind.unwrap_or_default();
        let field = |field: &'static str, hint: &str| TaskActionErr::KindField {
            name: name.to_owned(),
            kind: kind.as_str(),
            field,
            hint: hint.to_owned(),
        };
        if entry.overlap == Some(OverlapPolicy::Allow) {
            Self::refuse_allow(name, entry, kind)?;
        }
        if kind == LaunchKind::Shell {
            if entry.target.is_some() {
                return Err(field(
                    "target",
                    "a shell row runs `args` alone; re-add it with a kind flag",
                ));
            }
            if entry.args.is_empty() {
                return Err(TaskActionErr::MissingAction {
                    name: name.to_owned(),
                });
            }
            if entry.timeout.is_none() {
                return Err(TaskActionErr::MissingTimeout {
                    name: name.to_owned(),
                });
            }
            return Ok(Self::Shell(entry.args.clone()));
        }
        let Some(target) = entry.target.clone().filter(|target| !target.is_empty()) else {
            return Err(TaskActionErr::MissingTarget {
                name: name.to_owned(),
                kind: kind.as_str(),
            });
        };
        if entry.timeout.is_some() {
            return Err(field(
                "timeout",
                &format!("remove it; {}", kind.timeout_bound()),
            ));
        }
        Ok(Self::Launch(TaskLaunch {
            kind,
            target,
            worktree: entry.worktree.clone(),
            args: entry.args.clone(),
        }))
    }

    /// Refuses `overlap = 'allow'` on a row whose fires cannot run side by side.
    fn refuse_allow(name: &str, entry: &TaskEntry, kind: LaunchKind) -> Result<(), TaskActionErr> {
        let hint = match kind {
            LaunchKind::Shell => {
                "`allow` needs fires that run side by side, and a shell task holds one unfinished run per checkout"
            }
            LaunchKind::Workflow => {
                "`allow` needs fires that run side by side, and a workflow task holds one unfinished run per checkout"
            }
            LaunchKind::Team => {
                "`allow` needs fires that run side by side, and a team task's fires launch the same team into one lane"
            }
            LaunchKind::Agent if entry.dedupes_checkout() => {
                "`allow` launches later fires beside earlier ones, and `once-per-checkout` starts nothing more in a checkout this task launched in"
            }
            LaunchKind::Agent => return Ok(()),
        };
        Err(TaskActionErr::OverlapField {
            name: name.to_owned(),
            field: "overlap",
            hint,
        })
    }

    /// The command line as a shell would read it back, the launch as `rimz` would run it, or
    /// the delivery's handle.
    pub fn subject(&self) -> String {
        match self {
            Self::Shell(argv) => command_line(argv),
            Self::Launch(launch) => format!("rimz {}", command_line(&launch.argv())),
            Self::Deliver(target) => target.handle.clone(),
        }
    }

    pub const fn kind(&self) -> TaskActionKind {
        match self {
            Self::Shell(_) => TaskActionKind::Shell,
            Self::Launch(launch) => match launch.kind {
                LaunchKind::Shell => TaskActionKind::Shell,
                LaunchKind::Workflow => TaskActionKind::Workflow,
                LaunchKind::Agent => TaskActionKind::Agent,
                LaunchKind::Team => TaskActionKind::Team,
            },
            Self::Deliver(_) => TaskActionKind::Deliver,
        }
    }
}

/// One POSIX shell word that reads back as `word` exactly: bare when it holds only characters
/// no shell treats specially, else single-quoted.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c));
    if plain {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// An argv as one shell line whose words read back unchanged.
pub fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Errors from parsing or validating a schedule entry.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleErr {
    /// The stored row does not decode, so it has no trigger to read: the refusal is the error.
    #[error(transparent)]
    Refused(TaskActionErr),
    #[error("schedule `{name}` needs a firing time: set `cron = \"...\"`")]
    NoTime { name: String },
    #[error("schedule `{name}` sets conflicting schedule fields; use one of `cron` or `fire-at`")]
    TimeConflict { name: String },
    #[error(
        "schedule `{name}` sets conflicting trigger fields; use one of `when`, `signal`, `watch`, `cron`, or `fire-at`"
    )]
    TriggerConflict { name: String },
    #[error("schedule `{name}` sets `match` without `signal`")]
    MatchWithoutSignal { name: String },
    #[error(transparent)]
    Selector(#[from] crate::harness::signal::SelectorErr),
    #[error(
        "schedule `{name}` sets `repeat` without `signal` or `when`; `cron` repeats already, and other triggers fire once"
    )]
    RepeatWithoutSignal { name: String },
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
    #[error("schedule `{name}` has an empty watched command")]
    BadWatch { name: String },
    #[error(
        "schedule `{name}` has an invalid one-shot time `{value}`; use a delay above zero (`30m`, `2h`) or a clock time (`HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`)"
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
    #[error(transparent)]
    BadName(#[from] crate::harness::name::NameErr),
}

/// Runtime shape compiled once from one persisted task row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskShape {
    action: Result<TaskAction, TaskActionErr>,
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
        Self {
            action: TaskAction::from_entry(name, entry),
            trigger: parse_trigger(name, entry),
            ephemeral: ephemeral_lifetime(entry),
            kind: lifecycle_kind(entry),
        }
    }

    pub fn action(&self) -> Result<&TaskAction, &TaskActionErr> {
        self.action.as_ref()
    }

    pub fn trigger(&self) -> &Result<ParsedTrigger, ScheduleErr> {
        &self.trigger
    }

    /// Whether the row is a one-shot or deadline-bound, which the catalog keeps in the room's
    /// instance store rather than a task file.
    pub const fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }
}

fn lifecycle_kind(entry: &TaskEntry) -> TaskKind {
    if entry.wait.is_some() || entry.team.is_some() || entry.loop_task.is_some() {
        return TaskKind::WakeUp;
    }
    if !repeats(entry) || entry.fire_at.is_some() || entry.watch.is_some() {
        return TaskKind::OneOff;
    }
    TaskKind::Routine
}

/// Whether the row's trigger fires again after a fire: `cron` by nature, a `signal` or `when`
/// row only with `repeat` or a team binding's `team`, since team arming writes standing
/// bindings alone.
pub(crate) fn repeats(entry: &TaskEntry) -> bool {
    entry.cron.is_some()
        || ((entry.repeat || entry.team.is_some())
            && (entry.signal.is_some() || entry.when.is_some()))
}

fn ephemeral_lifetime(entry: &TaskEntry) -> bool {
    !repeats(entry) || entry.deadline.is_some() || entry.watch.is_some() || entry.fire_at.is_some()
}

/// `at` to the minute in the machine's time zone, as listings and wakes print an instant.
fn local_minute(at: Timestamp) -> String {
    at.to_zoned_datetime(crate::config::MachineConfig::load_lenient().time_zone())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// A parsed schedule: a cron expression, or the absolute instant of a `fire-at` row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Schedule {
    Cron(CronSpec),
    Instant(Timestamp),
}

impl Schedule {
    /// A short human description for listings.
    fn describe(&self) -> String {
        match self {
            Schedule::Cron(cron) => format!("cron `{}`", cron.source),
            Schedule::Instant(at) => format!("once at {}", local_minute(*at)),
        }
    }

    /// Whether this schedule is due now, given the last time its task was
    /// armed or fired. First-sight arming is owned by the elder firing module.
    fn due(&self, last_fire: Timestamp, now: &ZonedDateTime) -> bool {
        match self {
            Schedule::Cron(cron) => cron.due(last_fire, now),
            Schedule::Instant(at) => last_fire < *at && *at <= now.timestamp(),
        }
    }

    /// First occurrence after `last_fire`, evaluated from `now` in the
    /// configured local zone. A returned timestamp may be at or before `now`,
    /// which means the elder should fire on its next tick.
    fn next_after(&self, last_fire: Timestamp, now: &ZonedDateTime) -> Option<Timestamp> {
        match self {
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
            // An older RimZ's watcher emits `wait.<name>`.
            Self::Watch { .. }
                if [format!("shell.{task_name}"), format!("wait.{task_name}")]
                    .contains(&signal.name.as_str().to_owned()) =>
            {
                Deliver
            }
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

/// Why a task's own signal trigger would not deliver the signal a manual fire carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignalMiss {
    /// The name is outside the trigger's selector, written as the selector.
    Outside(String),
    /// The payload fails these `--match` fields, each written `key=value`.
    Unmatched(Vec<String>),
}

impl ParsedTrigger {
    /// Why this trigger would not deliver `signal`: `None` when it would, and on a trigger that
    /// is not a signal subscription.
    pub fn signal_miss(&self, signal: &crate::harness::signal::Signal) -> Option<SignalMiss> {
        use crate::harness::signal::SignalResolution::{Deliver, Excluded};
        let Trigger::Signal { selector, matches } = &self.trigger else {
            return None;
        };
        match crate::harness::signal::resolve_subscription(selector, matches, signal) {
            Deliver => None,
            Excluded => Some(SignalMiss::Unmatched(
                matches
                    .iter()
                    .filter(|(key, expected)| {
                        !crate::harness::signal::holds_match(signal, key, expected)
                    })
                    .map(|(key, expected)| format!("{key}={expected}"))
                    .collect(),
            )),
            _ => Some(SignalMiss::Outside(selector.to_string())),
        }
    }

    pub fn next_after(&self, now: &ZonedDateTime) -> Option<Timestamp> {
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
    /// Each unread key of an unmet condition with its reason, by [`Self::with_unread`].
    unread: BTreeMap<String, String>,
    /// The first unmet condition key unread because its checkout has no supported forge.
    no_forge: Option<String>,
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
        self.condition_label_with(&BTreeMap::new())
    }

    /// [`Self::condition_label`] with each unread key's reason in place of `unknown`.
    pub fn condition_label_with(&self, unread: &BTreeMap<String, String>) -> Option<String> {
        let readings = |readings: &[(String, Option<String>)]| {
            readings
                .iter()
                .map(|(key, value)| {
                    let value = value
                        .as_deref()
                        .or_else(|| unread.get(key).map(String::as_str))
                        .unwrap_or("unknown");
                    format!("{key}: {value}")
                })
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

    /// Keep an unmet condition's unread reasons, and the key unread for want of a supported
    /// forge, for the readers that show or decide on them; any other state keeps none.
    fn with_unread(mut self, unread: BTreeMap<String, String>, no_forge: Option<String>) -> Self {
        if matches!(
            self.state,
            TaskTimingState::Waiting { .. } | TaskTimingState::Unknown { .. }
        ) {
            self.unread = unread;
            self.no_forge = no_forge;
        }
        self
    }

    /// Each unread key of an unmet condition with its reason.
    pub fn unread(&self) -> &BTreeMap<String, String> {
        &self.unread
    }

    /// The first unmet condition key unread because its checkout has no supported forge.
    fn no_forge_key(&self) -> Option<&str> {
        self.no_forge.as_deref()
    }

    /// The condition state's label with each unread key's reason.
    pub fn condition_label(&self) -> Option<String> {
        self.state.condition_label_with(&self.unread)
    }

    pub fn evaluate(
        parsed: &Result<ParsedTrigger, ScheduleErr>,
        source: catalog::TaskSource,
        last_fire: Option<Timestamp>,
        arming: Option<&arming::Arming>,
        now: &ZonedDateTime,
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
            unread: BTreeMap::new(),
            no_forge: None,
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

/// The instant a one-shot delay fires: `delay` after `now`, rounded up to the next whole
/// minute, so its `once at` label names the minute the scheduler fires it.
fn delayed_instant(delay: Duration, now: Timestamp) -> anyhow::Result<Timestamp> {
    const MINUTE: i128 = 60_000_000_000;
    let target = now
        .checked_add(delay)
        .context("resolving the delay against the clock")?
        .as_nanosecond();
    let rounded = target.div_euclid(MINUTE) * MINUTE
        + if target.rem_euclid(MINUTE) == 0 {
            0
        } else {
            MINUTE
        };
    Timestamp::from_nanosecond(rounded).context("rounding the delay to the next scheduler minute")
}

/// Resolve a one-shot `at` to its instant in `now`'s zone: a delay (`30m`) to that long after
/// `now`, rounded up to the minute, and a clock time (`HH:MM`, `YYYY-MM-DD HH:MM`, or
/// `<weekday> HH:MM`) to its first instant strictly after `now`. A dated time at or before
/// `now` is refused as in the past.
pub fn resolve_once_at(
    name: &str,
    raw: &str,
    now: &ZonedDateTime,
) -> Result<Timestamp, ScheduleErr> {
    let bad = || ScheduleErr::BadOnceAt {
        name: name.to_owned(),
        value: raw.to_owned(),
    };
    let at = match OnceAt::parse(raw).ok_or_else(bad)? {
        OnceAt::Delay(delay) => return delayed_instant(delay, now.timestamp()).map_err(|_| bad()),
        OnceAt::Clock(at) => at,
    };
    let candidate = at.resolve(now).ok_or_else(bad)?;
    if matches!(at, CalendarAt::Dated(..)) && candidate <= now.timestamp() {
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
        TimingFields::Instant(at) => Schedule::Instant(at),
        TimingFields::Missing => {
            return Err(ScheduleErr::NoTime {
                name: name.to_owned(),
            });
        }
    };
    Ok(ParsedSchedule {
        schedule,
        once: entry.cron.is_none(),
    })
}

/// Parse and validate the single trigger encoded by a task row.
pub fn parse_trigger(name: &str, entry: &TaskEntry) -> Result<ParsedTrigger, ScheduleErr> {
    if let Some(refused) = &entry.refused {
        return Err(ScheduleErr::Refused(TaskActionErr::Refused {
            name: name.to_owned(),
            detail: refused.detail.clone(),
        }));
    }
    let has_schedule = entry.cron.is_some() || entry.fire_at.is_some();
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
    if entry.repeat && !has_signal && !has_when {
        return Err(ScheduleErr::RepeatWithoutSignal {
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
        once: matches!(trigger, Trigger::Watch(_))
            || (matches!(trigger, Trigger::Signal { .. } | Trigger::Condition { .. })
                && !repeats(entry)),
        trigger,
    })
}

fn validate_watch(name: &str, spec: &crate::config::WatchSpec) -> Result<(), ScheduleErr> {
    let crate::config::WatchSpec::Command(command) = spec;
    if command.trim().is_empty() {
        return Err(ScheduleErr::BadWatch {
            name: name.to_owned(),
        });
    }
    Ok(())
}

enum TimingFields<'a> {
    Conflict,
    Cron(&'a str),
    Instant(Timestamp),
    Missing,
}

impl<'a> TimingFields<'a> {
    fn classify(entry: &'a TaskEntry) -> Self {
        match (entry.fire_at, entry.cron.as_deref()) {
            (Some(_), Some(_)) => Self::Conflict,
            (Some(at), None) => Self::Instant(at),
            (None, Some(cron)) => Self::Cron(cron),
            (None, None) => Self::Missing,
        }
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

    fn matches(&self, now: &ZonedDateTime) -> bool {
        self.matches_day(now.date())
            && self.hours & 1 << now.hour() != 0
            && self.minutes & 1 << now.minute() != 0
    }

    fn due(&self, last_fire: Timestamp, now: &ZonedDateTime) -> bool {
        self.matches(now) && minute_bucket(last_fire) < minute_bucket(now.timestamp())
    }

    /// The first selected minute after `now` (or `now` itself while it is due), found day by
    /// day and then minute by minute within the first day that holds one.
    fn next_after(&self, last_fire: Timestamp, now: &ZonedDateTime) -> Option<Timestamp> {
        if self.due(last_fire, now) {
            return Some(now.timestamp());
        }
        let zone = now.time_zone();
        let from = (minute_bucket(now.timestamp()) + 1) * 60;
        let mut date = now.date();
        for _ in 0..CRON_SEARCH_DAYS {
            let next = date.tomorrow().ok()?;
            if self.matches_day(date) {
                let start = date
                    .to_zoned_datetime(zone.clone())
                    .ok()?
                    .timestamp()
                    .as_second();
                let end = next
                    .to_zoned_datetime(zone.clone())
                    .ok()?
                    .timestamp()
                    .as_second();
                let found = (start.max(from)..end).step_by(60).find_map(|second| {
                    let at = Timestamp::from_second(second).ok()?;
                    self.matches(&at.to_zoned_datetime(zone.clone()))
                        .then_some(at)
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

fn minute_bucket(timestamp: Timestamp) -> i64 {
    timestamp.as_second().div_euclid(60)
}

#[cfg(test)]
pub(super) mod tests;
