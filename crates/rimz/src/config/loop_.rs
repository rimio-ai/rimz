use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::ids::{AgentKind, AgentSessionId};
use crate::utils::time::{DurationUnit, parse_duration_units};

const DEFAULT_TIMEOUT_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
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
    pub tasks: Tasks,
    #[serde(skip)]
    pub skipped_tasks: Tasks,
}

impl LoopConfig {
    /// Read only loop.toml, with the full lenient load's defaults, TTL and quiet-stamp guard.
    pub(crate) fn load_lenient() -> Self {
        Self::load_lenient_from(&super::MachineConfig::loop_path())
    }

    fn load_lenient_from(path: &Path) -> Self {
        super::read_file_lenient(
            path,
            super::LOOP_MEMO.get_or_init(|| std::sync::Mutex::new(None)),
            super::parse_loop_text_collecting,
        )
        .as_ref()
        .clone()
    }

    pub fn is_empty(&self) -> bool {
        self.default_timeout.is_none() && self.tasks.0.is_empty()
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

/// How long an automated agent start may wait for capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum CapacityWait {
    Forever,
    Duration(Duration),
}

#[derive(Debug, thiserror::Error)]
pub enum CapacityWaitErr {
    #[error(transparent)]
    Duration(#[from] crate::utils::time::TimeInputErr),
    #[error("capacity-wait must be greater than zero, or forever")]
    Zero,
}

impl std::str::FromStr for CapacityWait {
    type Err = CapacityWaitErr;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw == "forever" {
            return Ok(Self::Forever);
        }
        let duration = parse_duration_units(raw, DEFAULT_TIMEOUT_UNITS)?;
        if duration.is_zero() {
            return Err(CapacityWaitErr::Zero);
        }
        Ok(Self::Duration(duration))
    }
}

impl TryFrom<String> for CapacityWait {
    type Error = CapacityWaitErr;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        raw.parse()
    }
}

impl From<CapacityWait> for String {
    fn from(wait: CapacityWait) -> Self {
        match wait {
            CapacityWait::Forever => "forever".to_owned(),
            CapacityWait::Duration(duration) => {
                crate::utils::time::format_duration_compact(duration)
            }
        }
    }
}

/// A workflow agent step's `throttle` key; absent means on.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThrottleSwitch {
    On,
    Off,
}

/// What a trigger does when its task is busy: it holds the run lock, or an earlier fire's
/// launch is still alive.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OverlapPolicy {
    #[default]
    Skip,
    Coalesce,
    Queue,
    Replace,
    /// An agent task launches beside its earlier fires' alive launches, keeping no trigger;
    /// a run-lock collision still records `overlapped`.
    Allow,
}

impl OverlapPolicy {
    /// Whether a busy task keeps its trigger waiting: `coalesce`, `queue` and `replace` do,
    /// while `skip` and `allow` keep none.
    pub const fn retains_triggers(self) -> bool {
        !matches!(self, Self::Skip | Self::Allow)
    }
}

/// Named loop tasks, ordered by name. A map keeps `rimz loop add/remove/run`
/// addressing one task by a stable name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Tasks(pub BTreeMap<String, TaskEntry>);

/// What a loop task's fire launches: a shell command, or a workflow, agent or team launch.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LaunchKind {
    #[default]
    Shell,
    Workflow,
    Agent,
    Team,
}

impl LaunchKind {
    /// The word a row's `kind` key, `rimz loop add`'s flag and the JSON views spell.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Workflow => "workflow",
            Self::Agent => "agent",
            Self::Team => "team",
        }
    }

    /// What bounds a fire of this kind in place of a shell task's `timeout`.
    pub const fn timeout_bound(self) -> &'static str {
        match self {
            Self::Shell => "the task's `timeout` bounds its command",
            Self::Workflow => "a workflow task's run is bounded by its workflow's deadline",
            Self::Agent => "an agent task's `-p` run takes `--timeout` in its arguments after `--`",
            Self::Team => "loop does not time a team task's launch",
        }
    }
}

/// One triggered loop wake-up: a trigger, its placement, and one action. `kind`, `target` and
/// `args` say what a fire launches in its checkout; `wait` delivers to a pinned session instead,
/// the row `rimz shell`, `rimz watch` and team arming write. Every key outside the row refuses
/// that row alone (`refused`), an older row's `command` included.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(remote = "Self", default, deny_unknown_fields)]
pub struct TaskEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What each fire launches; a row with `args` and no `kind` is a shell row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<LaunchKind>,
    /// The workflow, agent spec or team (with its `#lane`) a non-shell fire launches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// A shell row's argv, word for word; else the tail each fire hands its launch unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(rename = "each-worktree", skip_serializing_if = "std::ops::Not::not")]
    pub each_worktree: bool,
    /// The RimZ-owned worktree every fire runs in, `rimz loop add -w`: a launch's fire hands it
    /// to the launch as `-w`, and a shell fire creates or reuses it and runs its command there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<TaskTarget>,
    #[serde(rename = "wait-meta", skip_serializing_if = "Option::is_none")]
    pub wait_meta: Option<WaitMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<crate::ids::TeamInstanceId>,
    #[serde(rename = "loop-task", skip_serializing_if = "Option::is_none")]
    pub loop_task: Option<String>,
    /// The text a delivery row hands its session, written by team arming.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(rename = "max-strikes", skip_serializing_if = "Option::is_none")]
    pub max_strikes: Option<u32>,
    /// Whether the task fires at most once per checkout whose fire did not fail.
    #[serde(rename = "once-per-checkout", skip_serializing_if = "Option::is_none")]
    pub once_per_checkout: Option<bool>,
    /// A watch's polarity: which of its outcomes delivers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<CheckOn>,
    pub root: PathBuf,
    /// Directory the launch or watch runs in; the arming worktree, or `root` when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// How long a shell row's command may run; a self wait's check-in interval on a delivery row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// The signal a team role's binding fires on, the one row that takes it: a row without a
    /// binding's shape (`team` and no action of its own: no `kind`, `target` or `args`) that
    /// sets `signal` or `match` is refused.
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
    /// Whether a `when` row fires again after its first fire; a `cron` row repeats by nature,
    /// and so does a team binding, whose `team` makes it standing.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub repeat: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<Timestamp>,
    /// When a self wait ends unmet: a shell's command is killed, and a watch wakes its agent once
    /// naming its timeout and retires. Unlike `deadline`, which only gates a fire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
    /// The instant of an absolute one-shot, written by `rimz loop add --at` and the `schedule`
    /// tool's `at`, and by an older RimZ's self wait. A scheduled fire that records a result
    /// removes the row, whatever the result; the clock pass dispatches a fire that recorded none
    /// again, and a manual run leaves the row.
    #[serde(rename = "fire-at", skip_serializing_if = "Option::is_none")]
    pub fire_at: Option<Timestamp>,
    /// The provider whose windows the row's `window.*` terms read, recorded at add.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::ids::AgentKind>,
    /// The account whose usage windows the row's conditions read; unset follows the room.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<crate::ids::LoginName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlap: Option<OverlapPolicy>,
    #[serde(rename = "overlap-limit", skip_serializing_if = "Option::is_none")]
    pub overlap_limit: Option<usize>,
    /// The agents behind the booking, nearest first: the agent that ran `rimz loop add`, then
    /// each agent that started the one before it. Empty for a booking from a shell.
    #[serde(rename = "booked-by", skip_serializing_if = "Vec::is_empty")]
    pub booked_by: Vec<crate::agents::LaunchedBy>,
    /// Why the stored row does not decode, with the row as stored; such a row never fires and
    /// is written back unchanged.
    #[serde(skip)]
    pub refused: Option<RefusedRow>,
}

/// A stored row this RimZ does not take, kept as its decoded values so a rewrite of its file
/// keeps them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedRow {
    pub raw: serde_json::Value,
    pub detail: String,
}

impl<'de> Deserialize<'de> for TaskEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let decoded = match removed_trigger(&raw) {
            Some(detail) => Err(detail.to_owned()),
            None => TaskEntry::deserialize(&raw).map_err(|error| error.to_string()),
        };
        let decoded = decoded.and_then(|entry| {
            let binding = entry.team.is_some()
                && entry.kind.is_none()
                && entry.target.is_none()
                && entry.args.is_empty();
            let unbound_signal = !binding && (entry.signal.is_some() || entry.matches.is_some());
            if unbound_signal {
                return Err(SIGNAL_REMOVED.to_owned());
            }
            Ok(entry)
        });
        Ok(decoded.unwrap_or_else(|detail| {
            let root = raw
                .get("root")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_default();
            TaskEntry {
                root,
                refused: Some(RefusedRow { detail, raw }),
                ..TaskEntry::default()
            }
        }))
    }
}

/// Why a row with `signal` or `match` outside a team binding's shape is refused.
const SIGNAL_REMOVED: &str = "a task no longer fires on `signal`: fire on a state with `when`, or \
                              have a workflow `subscribe` to the signal";

/// Why a stored row carrying a trigger RimZ no longer takes is refused, naming its replacement.
fn removed_trigger(raw: &serde_json::Value) -> Option<&'static str> {
    raw.get("every").and_then(|_| removed_task_key("every"))
}

/// Why a task key that named a removed trigger is refused, naming its replacement: `every`,
/// `in`, and `signal` or `match`, which only a team binding's row still carries.
pub(crate) fn removed_task_key(key: &str) -> Option<&'static str> {
    match key {
        "every" => Some(
            "a task no longer fires on `every`: write the interval as a `cron` expression \
             (`*/30 * * * *` for every 30 minutes)",
        ),
        "in" => Some("a task no longer fires on `in`: write the delay as `rimz loop add --at`"),
        "signal" | "match" => Some(SIGNAL_REMOVED),
        _ => None,
    }
}

impl Serialize for TaskEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.refused {
            Some(refused) => refused.raw.serialize(serializer),
            None => TaskEntry::serialize(self, serializer),
        }
    }
}

impl TaskEntry {
    /// The kind a launch row fires: its `kind`, else shell. `None` on a delivery row.
    pub fn launch_kind(&self) -> Option<LaunchKind> {
        if self.wait.is_some() {
            return None;
        }
        Some(self.kind.unwrap_or_default())
    }

    pub fn dedupes_checkout(&self) -> bool {
        self.once_per_checkout.unwrap_or(false)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WaitMeta {
    pub armed_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
    /// Name of the arming agent (a launched child's petname), whose `out/` directory
    /// receives the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader: Option<String>,
    /// The deadline of a `join` that claimed this shell's outcome: until then its exit is
    /// recorded as joined instead of sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_until: Option<Timestamp>,
}

/// What a shell's detached watcher runs: a shell command, run once.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum WatchSpec {
    Command(String),
}

fn shell_command_preview(command: &str) -> std::borrow::Cow<'_, str> {
    if command.chars().count() <= 120 {
        return std::borrow::Cow::Borrowed(command);
    }
    let Some(start) = redirect_clause(command) else {
        return crate::theme::fmt::command_preview(command);
    };
    let prefix = command[..start].trim_end();
    let suffix = &command[start..];
    let budget = 120_usize.saturating_sub(suffix.chars().count() + 1).max(3);
    let count = prefix.chars().count();
    if count <= budget {
        return std::borrow::Cow::Borrowed(command);
    }
    let head = 60.min(count.saturating_sub(3));
    let tail = budget.saturating_sub(head + 1);
    let head = prefix.chars().take(head).collect::<String>();
    let tail = prefix.chars().skip(count - tail).collect::<String>();
    std::borrow::Cow::Owned(format!("{head}…{tail} {suffix}"))
}

fn redirect_clause(command: &str) -> Option<usize> {
    let mut operators = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut word_start = 0;
    let mut redirect_amp = None;
    let mut chars = command.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '#' if index == word_start => return None,
            '>' => {
                let start = if let Some(amp) = redirect_amp.take().filter(|amp| *amp + 1 == index) {
                    amp
                } else if &command[word_start..index] == "2" {
                    word_start
                } else {
                    index
                };
                let rest = &command[index..];
                let (length, target) = if rest.starts_with(">&1") {
                    (3, false)
                } else if rest.starts_with(">>") {
                    (2, true)
                } else {
                    (1, true)
                };
                let end = index + length;
                operators.push((start, end, target));
                while chars.next_if(|(next, _)| *next < end).is_some() {}
                word_start = end;
            }
            '&' if chars.peek().is_some_and(|(_, next)| *next == '>') => {
                redirect_amp = Some(index);
            }
            ';' | '|' | '&' | '<' | '(' | ')' | '\n' => {
                operators.clear();
                word_start = index + ch.len_utf8();
            }
            ch if ch.is_whitespace() => word_start = index + ch.len_utf8(),
            _ => {}
        }
    }
    let mut end = command.len();
    let mut start = None;
    for (operator, after, target) in operators.into_iter().rev() {
        let words = shlex::split(&command[after..end])?;
        if words.len() != usize::from(target) {
            break;
        }
        start = Some(operator);
        end = operator;
    }
    start
}

impl WatchSpec {
    /// The watch kind in wait JSON and trigger descriptions.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Command(_) => "shell",
        }
    }

    /// The trigger subject shared by wait lines, receipts, and messages.
    pub fn subject(&self) -> String {
        match self {
            Self::Command(command) => shell_command_preview(command).into_owned(),
        }
    }

    /// The trigger text of receipts and loop listings.
    pub fn describe(&self) -> String {
        format!("{}: {}", self.kind(), self.subject())
    }

    /// The first line of the delivered wait message.
    pub(crate) fn headline(&self) -> String {
        match self {
            Self::Command(command) => format!("waited on `{}`", shell_command_preview(command)),
        }
    }
}

impl TaskEntry {
    /// Root normalized for workspace identity. CLI-added tasks already store
    /// this shape; hand-edited machine roots may use `~`, but the loader skips
    /// empty and relative roots.
    pub fn resolved_root(&self) -> PathBuf {
        resolve_root_with(&self.root, home_dir())
    }

    /// Why a machine task's root cannot identify a workspace, if it cannot.
    pub fn root_problem(&self) -> Option<String> {
        if has_absolute_task_root(&self.root) {
            return None;
        }
        Some(if self.root.as_os_str().is_empty() {
            "no `root`".to_owned()
        } else {
            format!("relative `root` `{}`", self.root.display())
        })
    }

    pub(super) fn root_warning(&self, name: &str, path: &Path) -> Option<String> {
        let problem = self.root_problem()?;
        Some(format!(
            "loop task `{name}` in {} has {problem}; the task does not run; set `root` to an absolute path, or remove it with `rimz loop remove {name}`",
            path.display(),
        ))
    }

    pub fn run_dir(&self) -> PathBuf {
        self.dir
            .as_deref()
            .map(|dir| resolve_root_with(dir, home_dir()))
            .unwrap_or_else(|| self.resolved_root())
    }
}

/// A loop delivery target pinned to the agent that scheduled it. The handle is
/// display-only; `session` is the durable address.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaskTarget {
    /// The kind the agent ran as when it armed the target; a molt can change
    /// the kind of the session that receives the wake.
    pub kind: AgentKind,
    /// The agent's launch id, which every session of the agent carries across
    /// a restart or molt. A target armed before targets held launch ids holds
    /// the session id of the arming session instead, and follows only a swap
    /// within `kind`.
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

/// Whether a task root is absolute after home expansion, without filesystem access.
pub fn has_absolute_task_root(root: &Path) -> bool {
    expand_root_with(root, home_dir()).is_absolute()
}

fn resolve_root_with(root: &Path, home: PathBuf) -> PathBuf {
    let expanded = expand_root_with(root, home);
    expanded.canonicalize().unwrap_or(expanded)
}

fn expand_root_with(root: &Path, home: PathBuf) -> PathBuf {
    let raw = root.to_string_lossy();
    if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        root.to_path_buf()
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
#[path = "loop_tests.rs"]
mod tests;
