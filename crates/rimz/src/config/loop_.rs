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
mod tests {
    use super::*;

    #[test]
    fn watch_descriptions_preserve_prefixes_and_expose_subjects() {
        let cases = [
            (
                WatchSpec::Command("echo hello".into()),
                "watch",
                "echo hello",
                "watch: echo hello",
            ),
            (WatchSpec::Pid { pid: 123 }, "pid", "123", "pid 123"),
            (
                WatchSpec::Check {
                    check: "false".into(),
                    every: "1s".into(),
                    on: CheckOn::Success,
                },
                "check",
                "false",
                "check: false",
            ),
            (
                WatchSpec::Check {
                    check: "false".into(),
                    every: "2s".into(),
                    on: CheckOn::Fail,
                },
                "check",
                "false every 2s on fail",
                "check: false every 2s on fail",
            ),
            (
                WatchSpec::File {
                    file: "app.log".into(),
                    grep: None,
                    mark: None,
                },
                "file",
                "app.log",
                "file: app.log",
            ),
            (
                WatchSpec::File {
                    file: "app.log".into(),
                    grep: Some("ready".into()),
                    mark: None,
                },
                "file",
                "app.log grep: ready",
                "file: app.log grep: ready",
            ),
        ];
        for (spec, kind, subject, description) in cases {
            assert_eq!(spec.describe(), description);
            assert_eq!(spec.kind(), kind);
            assert_eq!(spec.subject(), subject);
        }
    }

    #[test]
    fn resolve_root_expands_tilde_prefix() {
        let home = PathBuf::from("/home/dev");
        assert_eq!(
            resolve_root_with(Path::new("~/workspace/app"), home.clone()),
            home.join("workspace/app")
        );
        assert_eq!(resolve_root_with(Path::new("~"), home.clone()), home);
        assert_eq!(
            resolve_root_with(Path::new("~other/app"), PathBuf::from("/home/dev")),
            PathBuf::from("~other/app")
        );
    }

    #[test]
    fn resolve_root_canonicalizes_existing_absolute_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("mkdir nested");
        let dotted = nested.join(".");

        assert_eq!(
            resolve_root_with(&dotted, PathBuf::from("/home/dev")),
            nested.canonicalize().expect("canonical nested")
        );
    }

    #[test]
    fn file_mark_tracks_absence_size_mtime_and_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        assert_eq!(FileMark::read(&path).unwrap(), None);
        std::fs::write(&path, "one").unwrap();
        let first = FileMark::read(&path).unwrap().unwrap();
        assert_eq!(first.size, 3);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        let rewritten = FileMark::read(&path).unwrap().unwrap();
        assert_eq!(rewritten.size, first.size);
        assert_ne!(rewritten, first);

        let replacement = dir.path().join("app.log.new");
        std::fs::write(&replacement, "one").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let replaced = FileMark::read(&path).unwrap().unwrap();
        assert_eq!(
            (replaced.size, replaced.modified),
            (rewritten.size, rewritten.modified)
        );
        assert_ne!(replaced, rewritten);
    }

    #[test]
    fn task_run_dir_defaults_to_root_and_resolves_explicit_directory() {
        let mut entry = TaskEntry {
            root: PathBuf::from("~/repo"),
            ..TaskEntry::default()
        };
        assert_eq!(entry.run_dir(), entry.resolved_root());
        assert!(serde_json::to_value(&entry).unwrap().get("dir").is_none());
        assert!(toml::Value::try_from(&entry).unwrap().get("dir").is_none());

        entry.dir = Some(PathBuf::from("~/linked"));
        assert_eq!(
            entry.run_dir(),
            resolve_root_with(Path::new("~/linked"), home_dir())
        );
    }

    #[test]
    fn task_entry_check_fields_round_trip_toml_and_json() {
        let deadline = Timestamp::from_second(1_783_000_000).expect("deadline");
        let entry = TaskEntry {
            wait: Some(TaskTarget {
                kind: AgentKind::new_unchecked("claude"),
                session: "sess-1".into(),
                handle: "@claude".to_owned(),
            }),
            wait_meta: Some(WaitMeta {
                armed_at: deadline,
                delay: Some("30m".to_owned()),
                reader: Some("coder".to_owned()),
            }),
            watch: Some(WatchSpec::Pid { pid: 16776 }),
            prompt: Some("wait".to_owned()),
            check: Some("cargo test".to_owned()),
            verify: Some("cargo xtask gate".to_owned()),
            max_attempts: Some(4),
            max_strikes: Some(5),
            on: Some(CheckOn::Success),
            root: PathBuf::from("/repo"),
            dir: Some(PathBuf::from("/linked")),
            every: Some("weekday".to_owned()),
            at: Some("07:00".to_owned()),
            budget: Some("$5.00".to_owned()),
            budget_per_day: Some("$20.00".to_owned()),
            surplus: Some("1.5x".to_owned()),
            surplus_after: Some("3d".to_owned()),
            signal: Some("ci.failed".to_owned()),
            matches: Some(BTreeMap::from([(
                "branch".to_owned(),
                "feature".to_owned(),
            )])),
            once: Some(true),
            deadline: Some(deadline),
            account: Some("work".parse().expect("login name")),
            ..TaskEntry::default()
        };
        let tasks = Tasks(BTreeMap::from([("ci".to_owned(), entry.clone())]));
        let loop_config = LoopConfig {
            tasks,
            ..LoopConfig::default()
        };

        let toml = toml::to_string(&loop_config).expect("toml");
        let toml_round: LoopConfig = toml::from_str(&toml).expect("toml round trip");
        assert_eq!(toml_round.tasks.0["ci"], entry);
        let mut legacy_toml: toml::Value = toml::from_str(&toml).expect("toml value");
        legacy_toml["tasks"]["ci"]["wait-meta"]
            .as_table_mut()
            .unwrap()
            .insert("pid".to_owned(), toml::Value::Integer(16776));
        legacy_toml["tasks"]["ci"]["watch"] =
            toml::Value::String("while kill -0 16776 2>/dev/null; do sleep 1; done".to_owned());
        let legacy_toml: LoopConfig = legacy_toml.try_into().expect("legacy toml");
        assert_eq!(
            legacy_toml.tasks.0["ci"].watch,
            Some(WatchSpec::Command(
                "while kill -0 16776 2>/dev/null; do sleep 1; done".to_owned()
            ))
        );
        assert_eq!(legacy_toml.tasks.0["ci"].wait_meta, entry.wait_meta);
        assert_eq!(toml_round.tasks.0["ci"].wait_meta, entry.wait_meta);
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.check.as_deref()),
            Some("cargo test")
        );
        assert_eq!(
            toml_round.tasks.0.get("ci").and_then(|entry| entry.on),
            Some(CheckOn::Success)
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.verify.as_deref()),
            Some("cargo xtask gate")
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.max_attempts),
            Some(4)
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.deadline),
            Some(deadline)
        );
        assert_eq!(
            toml_round
                .tasks
                .0
                .get("ci")
                .and_then(|entry| entry.every.as_deref()),
            Some("weekday")
        );
        assert!(
            toml.contains("every = \"weekday\""),
            "weekday cadence should round-trip through TOML: {toml}"
        );
        assert!(toml.contains("budget = \"$5.00\""), "{toml}");
        assert!(toml.contains("budget-per-day = \"$20.00\""), "{toml}");
        assert!(toml.contains("surplus = \"1.5x\""), "{toml}");
        assert!(toml.contains("surplus-after = \"3d\""), "{toml}");
        assert!(toml.contains("max-attempts = 4"), "{toml}");
        assert!(toml.contains("signal = \"ci.failed\""), "{toml}");
        assert!(toml.contains("[tasks.ci.match]"), "{toml}");
        assert!(toml.contains("branch = \"feature\""), "{toml}");
        assert!(toml.contains("once = true"), "{toml}");
        assert!(toml.contains("account = \"work\""), "{toml}");
        assert!(
            !toml::to_string(&TaskEntry::default())
                .expect("unpinned toml")
                .contains("account")
        );

        let json = serde_json::to_string(&loop_config.tasks).expect("json");
        let json_round: Tasks = serde_json::from_str(&json).expect("json round trip");
        assert_eq!(json_round.0.get("ci"), Some(&entry));
        let mut legacy = serde_json::to_value(&loop_config.tasks).expect("json value");
        assert_eq!(
            legacy["ci"]["wait"],
            serde_json::json!({"kind": "claude", "session": "sess-1", "handle": "@claude"})
        );
        assert!(legacy["ci"]["wait-meta"].get("armed_by").is_none());
        for armed_by in [
            serde_json::json!({"kind": "human"}),
            serde_json::json!({"kind": "agent", "handle": "@planner"}),
        ] {
            legacy["ci"]["wait-meta"]["armed_by"] = armed_by;
            let decoded: Tasks = serde_json::from_value(legacy.clone()).expect("legacy json");
            assert_eq!(decoded.0.get("ci"), Some(&entry));
        }
        assert_eq!(legacy["ci"]["watch"], serde_json::json!({"pid": 16776}));
        for spec in [
            WatchSpec::Check {
                check: "nc -z localhost 3000".to_owned(),
                every: "30s".to_owned(),
                on: CheckOn::Fail,
            },
            WatchSpec::File {
                file: PathBuf::from("/repo/app.log"),
                grep: Some("ready".to_owned()),
                mark: Some(FileMark {
                    size: 12,
                    modified: deadline,
                    dev: 2049,
                    ino: 131_074,
                }),
            },
            WatchSpec::File {
                file: PathBuf::from("/repo/app.log"),
                grep: None,
                mark: None,
            },
        ] {
            let json = serde_json::to_string(&spec).unwrap();
            assert_eq!(serde_json::from_str::<WatchSpec>(&json).unwrap(), spec);
            let toml = toml::to_string(&TaskEntry {
                watch: Some(spec.clone()),
                ..TaskEntry::default()
            })
            .unwrap();
            assert_eq!(
                toml::from_str::<TaskEntry>(&toml).unwrap().watch,
                Some(spec),
                "{toml}"
            );
        }
        legacy["ci"]["wait-meta"]["pid"] = serde_json::json!(16776);
        legacy["ci"]["watch"] = serde_json::json!("true");
        let decoded: Tasks = serde_json::from_value(legacy).expect("legacy json with pid");
        assert_eq!(
            decoded.0["ci"].watch,
            Some(WatchSpec::Command("true".to_owned()))
        );
        assert!(
            serde_json::to_value(&decoded).unwrap()["ci"]["wait-meta"]
                .get("pid")
                .is_none()
        );
    }

    #[test]
    fn default_timeout_accepts_task_duration_units_and_rejects_invalid_values() {
        let config: LoopConfig =
            toml::from_str("default-timeout = \"2h\"\n").expect("valid default timeout");
        assert_eq!(config.default_timeout.as_deref(), Some("2h"));

        let err = toml::from_str::<LoopConfig>("default-timeout = \"forever\"\n")
            .expect_err("invalid default timeout");
        assert!(err.to_string().contains("duration"), "{err}");
    }

    #[test]
    fn default_timeout_rejects_a_zero_duration() {
        for text in [
            "default-timeout = \"0s\"\n",
            "default-timeout = \"0m\"\n",
            "default-timeout = \"0d\"\n",
        ] {
            let err = toml::from_str::<LoopConfig>(text).expect_err("zero default timeout");
            assert!(
                err.to_string().contains("must be greater than zero"),
                "{err}"
            );
        }
    }

    #[test]
    fn throttle_table_parses_with_defaults_and_typed_values() {
        let defaults = LoopConfig::default().throttle;
        assert!(defaults.is_empty());
        assert_eq!(defaults.pace(), Duration::from_secs(10));
        assert_eq!(defaults.max_wait(), Duration::from_secs(30 * 60));
        assert_eq!(defaults.min_memory_bytes(), None);

        let config: LoopConfig = toml::from_str(
            "[throttle]\npace = \"0s\"\nmax-wait = \"2h\"\nmax-active = 12\n\
             max-active-per-task = 4\ncpu-pressure = 60\nio-pressure = 40\n\
             memory-pressure = 100\nmin-memory = \"8GB\"\nmin-disk = \"20GB\"\n",
        )
        .expect("valid throttle table");
        let throttle = &config.throttle;
        assert!(!config.is_empty(), "a throttle table alone is content");
        assert_eq!(throttle.pace(), Duration::ZERO);
        assert_eq!(throttle.max_wait(), Duration::from_secs(2 * 60 * 60));
        assert_eq!(throttle.max_active, Some(12));
        assert_eq!(throttle.max_active_per_task, Some(4));
        assert_eq!(
            (
                throttle.cpu_pressure,
                throttle.io_pressure,
                throttle.memory_pressure
            ),
            (Some(60), Some(40), Some(100))
        );
        assert_eq!(throttle.min_memory_bytes(), Some(8_000_000_000));
        assert_eq!(throttle.min_disk_bytes(), Some(20_000_000_000));
        let round_trip: LoopConfig =
            toml::from_str(&toml::to_string(&config).expect("serialize")).expect("reparse");
        assert_eq!(round_trip, config);
    }

    #[test]
    fn throttle_table_rejects_values_that_could_never_admit() {
        for (text, expected) in [
            ("max-wait = \"0s\"", "must be greater than zero"),
            ("max-wait = \"1d\"", "unit"),
            ("pace = \"soon\"", "duration"),
            ("max-active = 0", "must be greater than zero"),
            ("max-active-per-task = 0", "must be greater than zero"),
            ("cpu-pressure = 0", "between 1 and 100"),
            ("io-pressure = 101", "between 1 and 100"),
            ("min-memory = \"lots\"", "size"),
            ("min-disk = \"0\"", "must be greater than zero"),
        ] {
            let err =
                toml::from_str::<LoopConfig>(&format!("[throttle]\n{text}\n")).expect_err(text);
            assert!(err.to_string().contains(expected), "{text}: {err}");
        }
    }

    #[test]
    fn task_throttle_switch_round_trips_and_is_absent_by_default() {
        let tasks = toml::from_str::<Tasks>(
            "[exempt]\nroot = \"/repo\"\nthrottle = \"off\"\n[plain]\nroot = \"/repo\"\n",
        )
        .expect("parse");
        assert_eq!(tasks.0["exempt"].throttle, Some(ThrottleSwitch::Off));
        assert_eq!(tasks.0["plain"].throttle, None);
        let text = toml::to_string(&tasks).expect("serialize");
        assert!(text.contains("throttle = \"off\""), "{text}");
        assert_eq!(text.matches("throttle").count(), 1, "{text}");
        toml::from_str::<Tasks>("[bad]\nroot = \"/repo\"\nthrottle = \"maybe\"\n")
            .expect_err("only on and off");
    }

    #[test]
    fn unknown_task_keys_are_ignored() {
        let tasks =
            toml::from_str::<Tasks>("[old]\nspec = \"claude\"\nroot = \"/repo\"\n").expect("parse");
        assert_eq!(tasks.0["old"].root, PathBuf::from("/repo"));
        assert!(tasks.0["old"].agent.is_none());
    }

    #[test]
    fn task_budgets_validate_as_one_config_unit() {
        let invalid = TaskEntry {
            budget_per_day: Some("$20.00".to_owned()),
            ..TaskEntry::default()
        };
        assert!(matches!(
            invalid.validate_budget("nightly"),
            Err(TaskBudgetError::MissingRunBudget { task }) if task == "nightly"
        ));

        let malformed = TaskEntry {
            budget: Some("many dollars".to_owned()),
            ..TaskEntry::default()
        };
        assert!(matches!(
            malformed.validate_budget("nightly"),
            Err(TaskBudgetError::Invalid {
                field: "budget",
                ..
            })
        ));

        for (field, entry) in [
            (
                "surplus",
                TaskEntry {
                    surplus: Some("many".to_owned()),
                    ..TaskEntry::default()
                },
            ),
            (
                "surplus-after",
                TaskEntry {
                    surplus_after: Some("soon".to_owned()),
                    ..TaskEntry::default()
                },
            ),
        ] {
            assert!(matches!(
                entry.validate_budget("nightly"),
                Err(TaskBudgetError::InvalidSurplus { field: actual, .. }) if actual == field
            ));
        }

        assert!(matches!(
            TaskEntry {
                surplus: Some("1.5x".to_owned()),
                ..TaskEntry::default()
            }
            .validate_budget("nightly"),
            Err(TaskBudgetError::SurplusNeedsAgent { task }) if task == "nightly"
        ));
    }
}
