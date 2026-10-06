use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::ids::{AgentKind, AgentSessionId};
use crate::utils::size::parse_byte_size;
use crate::utils::time::{DurationUnit, parse_duration_units};

const DEFAULT_TIMEOUT_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

const THROTTLE_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
];

/// `loop.toml`: scheduled and automated agent-loop helpers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LoopConfig {
    #[serde(
        rename = "default-timeout",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_default_timeout"
    )]
    pub default_timeout: Option<String>,
    #[serde(skip_serializing_if = "ThrottleConfig::is_empty")]
    pub throttle: ThrottleConfig,
    pub tasks: Tasks,
}

impl LoopConfig {
    pub fn is_empty(&self) -> bool {
        self.default_timeout.is_none() && self.throttle.is_empty() && self.tasks.0.is_empty()
    }

    pub fn validate_budgets(&self) -> Result<(), TaskBudgetError> {
        for (name, entry) in &self.tasks.0 {
            entry.validate_budget(name)?;
        }
        Ok(())
    }
}

fn deserialize_default_timeout<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = raw.as_deref() {
        let duration =
            parse_duration_units(value, DEFAULT_TIMEOUT_UNITS).map_err(D::Error::custom)?;
        if duration.is_zero() {
            return Err(D::Error::custom("must be greater than zero"));
        }
    }
    Ok(raw)
}

/// `[throttle]`: how loop runs that spawn an agent take turns starting. Every
/// value is validated at deserialize; the accessors return the parsed form.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThrottleConfig {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_pace"
    )]
    pub pace: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_max_wait"
    )]
    pub max_wait: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_cap"
    )]
    pub max_active: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_cap"
    )]
    pub max_active_per_task: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_pressure"
    )]
    pub cpu_pressure: Option<u8>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_pressure"
    )]
    pub io_pressure: Option<u8>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_pressure"
    )]
    pub memory_pressure: Option<u8>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_size"
    )]
    pub min_memory: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_size"
    )]
    pub min_disk: Option<String>,
}

impl ThrottleConfig {
    pub const DEFAULT_PACE: &'static str = "10s";
    pub const DEFAULT_MAX_WAIT: &'static str = "30m";

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The longest one start holds the turn; zero turns pacing off.
    pub fn pace(&self) -> Duration {
        throttle_duration(self.pace.as_deref().unwrap_or(Self::DEFAULT_PACE))
            .unwrap_or(Duration::from_secs(10))
    }

    /// How long a run waits for its turn before it is skipped.
    pub fn max_wait(&self) -> Duration {
        throttle_duration(self.max_wait.as_deref().unwrap_or(Self::DEFAULT_MAX_WAIT))
            .unwrap_or(Duration::from_secs(30 * 60))
    }

    pub fn min_memory_bytes(&self) -> Option<u64> {
        parse_byte_size(self.min_memory.as_deref()?).ok()
    }

    pub fn min_disk_bytes(&self) -> Option<u64> {
        parse_byte_size(self.min_disk.as_deref()?).ok()
    }
}

fn throttle_duration(raw: &str) -> crate::utils::time::Result<Duration> {
    parse_duration_units(raw, THROTTLE_UNITS)
}

fn deserialize_throttle_pace<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = raw.as_deref() {
        throttle_duration(value).map_err(D::Error::custom)?;
    }
    Ok(raw)
}

fn deserialize_throttle_max_wait<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = raw.as_deref()
        && throttle_duration(value)
            .map_err(D::Error::custom)?
            .is_zero()
    {
        return Err(D::Error::custom("must be greater than zero"));
    }
    Ok(raw)
}

fn deserialize_throttle_cap<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let cap = Option::<u32>::deserialize(deserializer)?;
    if cap == Some(0) {
        return Err(D::Error::custom("must be greater than zero"));
    }
    Ok(cap)
}

fn deserialize_throttle_pressure<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let percent = Option::<u8>::deserialize(deserializer)?;
    if percent.is_some_and(|percent| !(1..=100).contains(&percent)) {
        return Err(D::Error::custom("must be a percentage between 1 and 100"));
    }
    Ok(percent)
}

fn deserialize_throttle_size<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = raw.as_deref()
        && parse_byte_size(value).map_err(D::Error::custom)? == 0
    {
        return Err(D::Error::custom("must be greater than zero"));
    }
    Ok(raw)
}

/// A task's own `throttle` key; absent means on.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThrottleSwitch {
    On,
    Off,
}

/// Named loop tasks, ordered by name. A map keeps `rimz loop add/remove/run`
/// addressing one task by a stable name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Tasks(pub BTreeMap<String, TaskEntry>);

/// One triggered loop wake-up. `agent` names a supervised turn or resident
/// layout; `wait` delivers to a pinned session.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaskEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stay: bool,
    #[serde(rename = "each-worktree", skip_serializing_if = "std::ops::Not::not")]
    pub each_worktree: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub takeover: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subscribe: Vec<super::TeamSignalBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<TaskTarget>,
    #[serde(rename = "wait-meta", skip_serializing_if = "Option::is_none")]
    pub wait_meta: Option<WaitMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<crate::ids::TeamInstanceId>,
    #[serde(rename = "loop-task", skip_serializing_if = "Option::is_none")]
    pub loop_task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(rename = "prompt-file", skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    #[serde(rename = "max-attempts", skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    #[serde(rename = "max-strikes", skip_serializing_if = "Option::is_none")]
    pub max_strikes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<CheckOn>,
    pub root: PathBuf,
    /// Directory for check/watch commands; the arming worktree, or `root` when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    #[serde(rename = "budget-per-day", skip_serializing_if = "Option::is_none")]
    pub budget_per_day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surplus: Option<String>,
    #[serde(rename = "surplus-after", skip_serializing_if = "Option::is_none")]
    pub surplus_after: Option<String>,
    #[serde(rename = "system-prompt-file", skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<Vec<String>>,
    #[serde(rename = "for", skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub matches: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub once: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<Timestamp>,
    /// The instant of an absolute one-shot, written by `rimz loop add --after-reset`.
    #[serde(rename = "fire-at", skip_serializing_if = "Option::is_none")]
    pub fire_at: Option<Timestamp>,
    /// The provider whose windows the row's `window.*` terms read, recorded at add.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::ids::AgentKind>,
    /// The provider account every fire of an `agent` task runs on; unset follows the room.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<crate::ids::LoginName>,
    /// `off` exempts every fire of the task from the start throttle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throttle: Option<ThrottleSwitch>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WaitMeta {
    pub armed_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
    /// Handle of the arming agent, whose `out/` directory receives the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader: Option<String>,
}

/// What a wait's detached watcher observes. A bare string is a shell command
/// run once, the form hand-written rows carry; every other form is probed by
/// the watcher until it is met.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum WatchSpec {
    Command(String),
    Pid {
        pid: u32,
    },
    /// A shell predicate run every `every` (a duration label) until its exit
    /// matches `on`, `success` or `fail`.
    Check {
        check: String,
        every: String,
        on: CheckOn,
    },
    /// A file's changes from `mark`, its state when armed (`None`: absent);
    /// with `grep`, only a new line containing the literal pattern counts.
    File {
        file: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grep: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mark: Option<FileMark>,
    },
}

/// The stat of a watched file that a later stat compares against. `dev` and
/// `ino` identify the file, so a replacement at the same path is a change.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FileMark {
    pub size: u64,
    pub modified: Timestamp,
    pub dev: u64,
    pub ino: u64,
}

impl FileMark {
    /// The stat a `--file` watch compares against; `None` when the path is absent.
    pub fn read(path: &Path) -> std::io::Result<Option<Self>> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let modified = Timestamp::try_from(metadata.modified()?).map_err(std::io::Error::other)?;
        Ok(Some(Self {
            size: metadata.len(),
            modified,
            dev: std::os::unix::fs::MetadataExt::dev(&metadata),
            ino: std::os::unix::fs::MetadataExt::ino(&metadata),
        }))
    }
}

impl WatchSpec {
    /// The `TYPE` word of `rimz wait list`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Command(_) => "watch",
            Self::Pid { .. } => "pid",
            Self::Check { .. } => "check",
            Self::File { .. } => "file",
        }
    }

    /// The trigger text without its kind prefix, as `rimz wait list` shows it.
    pub fn subject(&self) -> String {
        match self {
            Self::Command(command) => crate::theme::fmt::command_preview(command).into_owned(),
            Self::Pid { pid } => pid.to_string(),
            Self::Check { check, every, on } => {
                let mut text = crate::theme::fmt::command_preview(check).into_owned();
                if every != "1s" {
                    text.push_str(&format!(" every {every}"));
                }
                if *on == CheckOn::Fail {
                    text.push_str(" on fail");
                }
                text
            }
            Self::File { file, grep, .. } => match grep {
                Some(grep) => format!(
                    "{} grep: {}",
                    file.display(),
                    crate::theme::fmt::command_preview(grep)
                ),
                None => file.display().to_string(),
            },
        }
    }

    /// The trigger text of receipts and loop listings.
    pub fn describe(&self) -> String {
        let separator = if matches!(self, Self::Pid { .. }) {
            " "
        } else {
            ": "
        };
        format!("{}{separator}{}", self.kind(), self.subject())
    }

    /// The first line of the delivered wait message.
    pub(crate) fn headline(&self) -> String {
        match self {
            Self::Command(command) => format!(
                "waited on `{}`",
                crate::theme::fmt::command_preview(command)
            ),
            Self::Pid { pid } => format!("waited on pid {pid}"),
            Self::Check { check, .. } => format!(
                "waited on check `{}`",
                crate::theme::fmt::command_preview(check)
            ),
            Self::File { file, grep, .. } => match grep {
                Some(grep) => format!(
                    "waited on file {} for `{}`",
                    file.display(),
                    crate::theme::fmt::command_preview(grep)
                ),
                None => format!("waited on file {}", file.display()),
            },
        }
    }
}

impl TaskEntry {
    /// Root normalized for workspace identity. CLI-added tasks
    /// already store this shape; hand-edited tasks may use `~` or a relative
    /// path.
    pub fn resolved_root(&self) -> PathBuf {
        resolve_root_with(&self.root, home_dir())
    }

    pub fn run_dir(&self) -> PathBuf {
        self.dir
            .as_deref()
            .map(|dir| resolve_root_with(dir, home_dir()))
            .unwrap_or_else(|| self.resolved_root())
    }

    pub fn validate_budget(&self, task: &str) -> Result<(), TaskBudgetError> {
        if let Some(raw) = self.budget.as_deref() {
            raw.parse::<crate::harness::budget::BudgetSpec>()
                .map_err(|source| TaskBudgetError::Invalid {
                    task: task.to_owned(),
                    field: "budget",
                    source,
                })?;
        }
        if let Some(raw) = self.budget_per_day.as_deref() {
            raw.parse::<crate::harness::budget::BudgetSpec>()
                .map_err(|source| TaskBudgetError::Invalid {
                    task: task.to_owned(),
                    field: "budget-per-day",
                    source,
                })?;
            if self.budget.is_none() {
                return Err(TaskBudgetError::MissingRunBudget {
                    task: task.to_owned(),
                });
            }
        }
        if let Some(raw) = self.surplus.as_deref() {
            crate::harness::schedule::parse_surplus(raw).map_err(|detail| {
                TaskBudgetError::InvalidSurplus {
                    task: task.to_owned(),
                    field: "surplus",
                    detail,
                }
            })?;
        }
        if let Some(raw) = self.surplus_after.as_deref() {
            crate::harness::schedule::parse_surplus_after(raw).map_err(|detail| {
                TaskBudgetError::InvalidSurplus {
                    task: task.to_owned(),
                    field: "surplus-after",
                    detail,
                }
            })?;
        }
        if (self.surplus.is_some() || self.surplus_after.is_some())
            && self.agent.is_none()
            && self.wait.is_none()
        {
            return Err(TaskBudgetError::SurplusNeedsAgent {
                task: task.to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TaskBudgetError {
    #[error("task `{task}` has invalid `{field}`: {source}")]
    Invalid {
        task: String,
        field: &'static str,
        #[source]
        source: crate::harness::budget::BudgetParseError,
    },
    #[error("task `{task}` sets `budget-per-day` without `budget`; set a per-run budget")]
    MissingRunBudget { task: String },
    #[error("task `{task}` has invalid `{field}`: {detail}")]
    InvalidSurplus {
        task: String,
        field: &'static str,
        detail: String,
    },
    #[error("task `{task}` sets a surplus gate without `agent` or `wait`")]
    SurplusNeedsAgent { task: String },
}

/// A loop delivery target pinned to the exact live agent session that scheduled
/// it. The handle is display-only; `session` is the durable address.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaskTarget {
    pub kind: AgentKind,
    pub session: AgentSessionId,
    pub handle: String,
}

impl Default for TaskTarget {
    fn default() -> Self {
        Self {
            kind: crate::ids::AgentKind::new_unchecked(""),
            session: crate::ids::AgentSessionId::from(""),
            handle: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckOn {
    #[default]
    Fail,
    Success,
    Any,
}

fn resolve_root_with(root: &Path, home: PathBuf) -> PathBuf {
    let raw = root.to_string_lossy();
    let expanded = if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        root.to_path_buf()
    };
    expanded.canonicalize().unwrap_or(expanded)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
#[path = "loop_tests.rs"]
mod tests;
