use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::definition::Definition;
use crate::harness::signal::{
    SelectorErr, SignalSelector, is_match_wildcard, parse_signal_selector,
};
use crate::harness::throttle::Limit;
use crate::store::workflow::{RunRecord, StepLifecycle};

pub use crate::store::workflow::ScopeKind;

pub(super) type Subscription = (SignalSelector, BTreeMap<String, String>);

mod kinds;

pub use kinds::WorkflowFile;

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

pub(super) fn issue(path: &str, message: impl Display) -> Issue {
    Issue {
        path: path.into(),
        message: format!("{path}: {message}"),
    }
}

/// The compiled model of a definition: each kind table lowered into a check or a step, as
/// the engine and its readers consume them. The authored file is [`kinds::WorkflowFile`].
#[derive(Clone, Debug)]
pub struct WorkflowSpec {
    pub workflow: Header,
    pub checks: BTreeMap<String, Check>,
    pub steps: BTreeMap<String, Step>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Header {
    pub summary: Option<String>,
    pub scope: ScopeKind,
    pub start: String,
    pub deadline: Option<String>,
    pub limit: Option<Limit>,
    #[serde(rename = "on-break")]
    pub(super) on_break: Option<OnBreak>,
    #[serde(default)]
    pub on: HeaderEdges,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged, rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub(super) enum OnBreak {
    Target(String),
    Agent {
        agent: String,
        #[serde(rename = "prompt-file")]
        prompt_file: Option<String>,
    },
}

impl<'de> Deserialize<'de> for OnBreak {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OnBreak;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("@failed, @done, or a table with agent and optional prompt-file")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<OnBreak, E> {
                Ok(OnBreak::Target(value.into()))
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<OnBreak, M::Error> {
                #[derive(Deserialize)]
                #[serde(rename_all = "kebab-case", deny_unknown_fields)]
                struct Handler {
                    agent: String,
                    prompt_file: Option<String>,
                }
                let handler =
                    Handler::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(OnBreak::Agent {
                    agent: handler.agent,
                    prompt_file: handler.prompt_file,
                })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct HeaderEdges {
    pub deadline: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct StepEdges {
    pub asked: Option<String>,
    pub cap: Option<String>,
    pub capacity: Option<String>,
    pub invalid: Option<String>,
    pub undelivered: Option<String>,
    /// An agent step's ending that died, timed out, or was limited; without it, `then` takes it.
    pub failed: Option<String>,
}

/// The kind table a check was written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckTable {
    Test,
    Script,
    Judge,
    Condition,
    Sleep,
}

/// The kind table a step was written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepTable {
    Agent,
    Command,
    Message,
}

impl CheckTable {
    pub fn word(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Script => "script",
            Self::Judge => "judge",
            Self::Condition => "condition",
            Self::Sleep => "sleep",
        }
    }
}

impl StepTable {
    pub fn word(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Command => "command",
            Self::Message => "message",
        }
    }
}

#[cfg(test)]
impl Check {
    /// A condition check lowered from its `[condition.<name>]` body, for tests.
    pub(in crate::harness::workflow) fn condition(body: &str) -> Self {
        let node: toml::Table = toml::from_str(body).expect("condition body parses");
        let file = toml::Table::from_iter([
            (
                "workflow".to_owned(),
                toml::Table::from_iter([
                    ("scope".to_owned(), "worktree".into()),
                    ("start".to_owned(), "c".into()),
                ])
                .into(),
            ),
            (
                "condition".to_owned(),
                toml::Table::from_iter([("c".to_owned(), node.into())]).into(),
            ),
        ]);
        let file: WorkflowFile = file.try_into().expect("condition table parses");
        let mut spec = file.lower().expect("condition lowers");
        spec.checks.remove("c").expect("lowered condition")
    }
}

/// A check as the definition compiles it, from its kind table.
#[derive(Clone, Debug)]
pub struct Check {
    pub table: CheckTable,
    pub summary: Option<String>,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub agent: Option<String>,
    pub sleep: Option<String>,
    pub when: Option<Vec<String>>,
    pub hold: Option<String>,
    pub then: Option<String>,
    pub prompt_file: Option<String>,
    pub window: Option<String>,
    pub(super) wake: Option<Vec<Wake>>,
    pub cases: Vec<Case>,
    pub else_outcome: Option<String>,
    pub provider: Option<crate::ids::AgentKind>,
    pub account: Option<crate::ids::LoginName>,
    pub on: BTreeMap<String, String>,
    pub timeout: Option<String>,
}

#[derive(Clone, Debug, JsonSchema)]
#[serde(untagged)]
#[schemars(deny_unknown_fields)]
pub(super) enum Wake {
    Selector(String),
    Binding {
        signal: String,
        #[serde(default, rename = "match")]
        matches: BTreeMap<String, String>,
        /// Text a session's leader gets after the forwarded signal; steps only.
        #[serde(default)]
        prompt: Option<String>,
    },
}

impl<'de> Deserialize<'de> for Wake {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Wake;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a signal selector or a table with signal and optional match")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Wake, E> {
                Ok(Wake::Selector(value.into()))
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<Wake, M::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Binding {
                    signal: String,
                    #[serde(default, rename = "match")]
                    matches: BTreeMap<String, String>,
                    #[serde(default)]
                    prompt: Option<String>,
                }
                let binding =
                    Binding::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(Wake::Binding {
                    signal: binding.signal,
                    matches: binding.matches,
                    prompt: binding.prompt,
                })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl Wake {
    /// A check's wake, which lifecycle signals never reach.
    pub(super) fn subscription(&self) -> Result<Subscription, SpecErr> {
        let (selector, matches) = self.parsed()?;
        if selector.family() == "agent"
            || (selector.family() == "team" && selector.to_string() != "team.stage")
        {
            return Err(SpecErr::LifecycleWake);
        }
        Ok((selector, matches))
    }

    /// A session's subscription, which may hear lifecycle signals, though never the
    /// leader's own: an `agent` selector names another agent by handle or session.
    pub(super) fn step_subscription(&self) -> Result<Subscription, SpecErr> {
        let (selector, matches) = self.parsed()?;
        let names_another = ["handle", "session"].iter().any(|key| {
            matches
                .get(*key)
                .is_some_and(|value| !value.is_empty() && !is_match_wildcard(value))
        });
        if selector.family() == "agent" && !names_another {
            return Err(SpecErr::SelfSignal);
        }
        Ok((selector, matches))
    }

    fn parsed(&self) -> Result<Subscription, SpecErr> {
        let (raw, matches) = match self {
            Self::Selector(signal) => (signal, BTreeMap::new()),
            Self::Binding {
                signal, matches, ..
            } => (signal, matches.clone()),
        };
        let selector = parse_signal_selector("workflow wake", raw, Some(&matches))?;
        Ok((selector, matches))
    }

    pub(super) fn selector(&self) -> &str {
        match self {
            Self::Selector(signal) | Self::Binding { signal, .. } => signal,
        }
    }

    pub(super) fn prompt(&self) -> Option<&str> {
        match self {
            Self::Selector(_) => None,
            Self::Binding { prompt, .. } => prompt.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Case {
    pub when: Vec<String>,
    pub outcome: String,
    #[serde(rename = "for")]
    pub hold: Option<String>,
}

pub(super) fn nonempty_cases<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Case>, D::Error> {
    let cases = Vec::<Case>::deserialize(deserializer)?;
    if cases.is_empty() {
        return Err(serde::de::Error::custom("cases must be nonempty"));
    }
    Ok(cases)
}

/// How a command node runs its command: as an argv or under `sh -c`.
pub enum Invoke<'a> {
    Run(&'a str),
    Shell(&'a str),
}

pub enum CheckKind<'a> {
    Test(Invoke<'a>),
    Script(Invoke<'a>),
    Judge(&'a str),
    Condition,
    Sleep,
}

/// The one command a `run` or `shell` pair names; none when both or neither is set.
fn invoke<'a>(run: Option<&'a str>, shell: Option<&'a str>) -> Option<Invoke<'a>> {
    match (run, shell) {
        (Some(run), None) => Some(Invoke::Run(run)),
        (None, Some(shell)) => Some(Invoke::Shell(shell)),
        _ => None,
    }
}

impl Check {
    pub fn kind(&self) -> Option<CheckKind<'_>> {
        let invoke = || invoke(self.run.as_deref(), self.shell.as_deref());
        Some(match self.table {
            CheckTable::Test => CheckKind::Test(invoke()?),
            CheckTable::Script => CheckKind::Script(invoke()?),
            CheckTable::Judge => CheckKind::Judge(self.agent.as_deref()?),
            CheckTable::Condition if self.cases.is_empty() => return None,
            CheckTable::Condition => CheckKind::Condition,
            CheckTable::Sleep => CheckKind::Sleep,
        })
    }

    pub fn is_sleep(&self) -> bool {
        matches!(self.kind(), Some(CheckKind::Sleep))
    }

    pub fn condition_cases(&self) -> std::borrow::Cow<'_, [Case]> {
        match self.kind() {
            Some(CheckKind::Condition) => std::borrow::Cow::Borrowed(&self.cases),
            Some(CheckKind::Sleep) => match &self.when {
                Some(when) => std::borrow::Cow::Owned(vec![Case {
                    when: when.clone(),
                    outcome: "then".into(),
                    hold: self.hold.clone(),
                }]),
                None => std::borrow::Cow::Borrowed(&[]),
            },
            _ => std::borrow::Cow::Borrowed(&[]),
        }
    }

    pub(super) fn fallback_outcome(&self) -> Option<&str> {
        if self.is_sleep() {
            Some("sleeping")
        } else {
            self.else_outcome.as_deref()
        }
    }
}

pub(super) const RESERVED_OUTCOMES: [&str; 7] = [
    "timeout", "invalid", "cap", "capacity", "deadline", "unknown", "hold",
];

/// A step as the definition compiles it, from its kind table.
#[derive(Clone, Debug)]
pub struct Step {
    pub table: StepTable,
    pub summary: Option<String>,
    pub agent: Option<String>,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub message: Option<String>,
    pub lifecycle: Option<crate::store::workflow::StepLifecycle>,
    /// A session's checks, in order, each of its clean rests enters at the first.
    pub rest_check: Option<Vec<String>>,
    pub keep: Option<bool>,
    pub notes: Option<bool>,
    pub prompt_file: Option<String>,
    pub window: Option<String>,
    pub occupancy: Option<OccupancyPolicy>,
    pub(super) subscribe: Option<Vec<Wake>>,
    pub throttle: Option<crate::config::ThrottleSwitch>,
    pub capacity_wait: Option<crate::config::CapacityWait>,
    pub limit: Option<Limit>,
    pub then: Vec<String>,
    pub on: StepEdges,
    pub max_visits: Option<u64>,
    pub timeout: Option<String>,
    /// The account an agent step's launch runs on; unset follows the profile and the room.
    pub account: Option<crate::ids::LoginName>,
    pub mode: Option<crate::agents::PermissionMode>,
    pub effort: Option<String>,
    pub fast: Option<bool>,
    /// A per-launch dollar budget, in the `--budget` grammar.
    pub budget: Option<String>,
    pub system_prompt_file: Option<String>,
    /// Launch the agent in a new worktree of the run's checkout under this name.
    pub worktree: Option<String>,
    /// The agent session a message step delivers to; another session under the address is
    /// undelivered.
    pub session: Option<crate::ids::AgentSessionId>,
}

/// What an agent step does about agents already in its checkout: wait for them to leave,
/// stop an idle one, or launch beside them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OccupancyPolicy {
    #[default]
    Wait,
    Takeover,
    Share,
}

impl OccupancyPolicy {
    pub fn word(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Takeover => "takeover",
            Self::Share => "share",
        }
    }
}

/// An agent step's launch settings beside its profile, as a fresh launch applies them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaunchOverrides {
    pub account: Option<crate::ids::LoginName>,
    pub mode: Option<crate::agents::PermissionMode>,
    pub effort: Option<String>,
    pub fast: Option<bool>,
    pub budget: Option<crate::harness::budget::BudgetSpec>,
    /// Resolved inside the workflow directory.
    pub system_prompt_file: Option<PathBuf>,
    pub worktree: Option<String>,
}

pub enum StepKind<'a> {
    Agent(&'a str),
    Command(Invoke<'a>),
    Message(&'a str),
}

impl Step {
    /// An agent step's lifecycle, which its table requires; every other step is a turn.
    pub fn lifecycle(&self) -> StepLifecycle {
        self.lifecycle.unwrap_or(StepLifecycle::Turn)
    }

    /// The step's `max-visits` raised by the run's grant; none when the step sets no cap.
    pub fn visit_cap(&self, grant: u64) -> Option<u64> {
        Some(self.max_visits?.saturating_add(grant))
    }

    /// The visits used and the cap, when the record's current step has spent its cap.
    pub fn tripped_visit_bound(&self, record: &RunRecord) -> Option<(u64, u64)> {
        let used = record.visits_to(&record.node);
        self.visit_cap(record.visit_grant)
            .filter(|max| used >= *max)
            .map(|max| (used, max))
    }

    /// The step's launch overrides, its system-prompt file resolved inside `dir`, with the
    /// budget its definition parsed.
    pub fn launch_overrides(
        &self,
        dir: &Path,
        budget: Option<&crate::harness::budget::BudgetSpec>,
    ) -> Result<LaunchOverrides, SpecErr> {
        Ok(LaunchOverrides {
            account: self.account.clone(),
            mode: self.mode,
            effort: self.effort.clone(),
            fast: self.fast,
            budget: budget.cloned(),
            system_prompt_file: self
                .system_prompt_file
                .as_deref()
                .map(|path| confined_file(dir, path))
                .transpose()?,
            worktree: self.worktree.clone(),
        })
    }

    pub fn kind(&self) -> Option<StepKind<'_>> {
        Some(match self.table {
            StepTable::Agent => StepKind::Agent(self.agent.as_deref()?),
            StepTable::Command => {
                StepKind::Command(invoke(self.run.as_deref(), self.shell.as_deref())?)
            }
            StepTable::Message => StepKind::Message(self.message.as_deref()?),
        })
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum SpecErr {
    #[error(
        "{path}: {target} is not a route target; route to a node, @done, or @failed, \
         or set [workflow] on-break"
    )]
    ReservedTarget { path: String, target: String },
    #[error("{path}: @break is reserved for workflow.on-break; choose another node name")]
    ReservedNode { path: String },
    #[error("{path}: {other}.{name} has the same name; a node name is unique across every kind")]
    DuplicateNode {
        path: String,
        name: String,
        other: &'static str,
    },
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::sync::Arc<std::io::Error>,
    },
    #[error("{path}: {}", if .source.span().is_some() {
        .source.to_string()
    } else {
        .source.message().to_owned()
    })]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid workflow path {path}: {reason}")]
    Path { path: PathBuf, reason: &'static str },
    #[error("invalid run command: use a nonempty, correctly quoted argv")]
    Command,
    #[error(transparent)]
    Window(#[from] crate::harness::when::WhenError),
    #[error("only provider-window keys are allowed")]
    WindowKeys,
    #[error(transparent)]
    Signal(#[from] SelectorErr),
    #[error("lifecycle signals cannot wake workflows; use team.stage or a non-lifecycle selector")]
    LifecycleWake,
    #[error(
        "an agent.* subscription needs match handle=<other> or session=<other>, so the leader is not woken by its own lifecycle signal"
    )]
    SelfSignal,
}

pub fn load(dir: &Path) -> Result<Definition, SpecErr> {
    let path = crate::disk::paths::workflow_spec_file(dir);
    let text = std::fs::read_to_string(&path).map_err(|source| SpecErr::Io {
        path,
        source: source.into(),
    })?;
    parse(&text)
}

pub(crate) fn parse(text: &str) -> Result<Definition, SpecErr> {
    let value: toml::Value = toml::from_str(text).map_err(|source| SpecErr::Toml {
        path: "workflow".into(),
        source,
    })?;
    reject_reserved_targets(&value)?;
    let file = kinds::deserialize(value)?;
    let mut spec = file.lower()?;
    if let Some(OnBreak::Agent { agent, prompt_file }) = &spec.workflow.on_break {
        spec.steps.insert(
            "@break".into(),
            Step {
                table: StepTable::Agent,
                summary: Some("Investigate the workflow failure with the user".into()),
                agent: Some(agent.clone()),
                prompt_file: prompt_file.clone(),
                lifecycle: Some(StepLifecycle::Session),
                rest_check: None,
                then: vec!["@failed".into()],
                on: StepEdges {
                    invalid: Some("@failed".into()),
                    ..Default::default()
                },
                run: None,
                shell: None,
                message: None,
                keep: None,
                notes: None,
                window: None,
                occupancy: None,
                subscribe: None,
                throttle: None,
                capacity_wait: None,
                limit: None,
                max_visits: None,
                timeout: None,
                account: None,
                mode: None,
                effort: None,
                fast: None,
                budget: None,
                system_prompt_file: None,
                worktree: None,
                session: None,
            },
        );
    }
    Ok(Definition::compile(spec))
}

fn reject_reserved_targets(value: &toml::Value) -> Result<(), SpecErr> {
    if let Some(header) = value.get("workflow") {
        for key in ["start", "on-break"] {
            reject_reserved_target(
                &format!("workflow.{key}"),
                header.get(key).and_then(toml::Value::as_str),
            )?;
        }
        reject_reserved_target(
            "workflow.on.deadline",
            header
                .get("on")
                .and_then(|on| on.get("deadline"))
                .and_then(toml::Value::as_str),
        )?;
    }
    for kind in kinds::KINDS {
        let Some(nodes) = value.get(kind).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, node) in nodes {
            let path = format!("{kind}.{name}");
            if name == "@break" {
                return Err(SpecErr::ReservedNode { path });
            }
            if let Some(then) = node.get("then") {
                reject_reserved_target(&format!("{path}.then"), then.as_str())?;
                for target in then.as_array().into_iter().flatten() {
                    reject_reserved_target(&format!("{path}.then"), target.as_str())?;
                }
            }
            let Some(on) = node.get("on").and_then(toml::Value::as_table) else {
                continue;
            };
            for (outcome, route) in on {
                reject_reserved_target(&format!("{path}.on.{outcome}"), route.as_str())?;
            }
        }
    }
    Ok(())
}

fn reject_reserved_target(path: &str, target: Option<&str>) -> Result<(), SpecErr> {
    if let Some(target) = target.filter(|target| matches!(*target, "@user" | "@break")) {
        return Err(SpecErr::ReservedTarget {
            path: path.into(),
            target: target.into(),
        });
    }
    Ok(())
}

pub fn schema() -> schemars::Schema {
    schemars::schema_for!(WorkflowFile)
}

pub(super) fn confined_file(dir: &Path, path: &str) -> Result<PathBuf, SpecErr> {
    if path.is_empty()
        || !Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(SpecErr::Path {
            path: path.into(),
            reason: "use a relative path with only normal components",
        });
    }
    let file = dir.join(path);
    let file = file.canonicalize().map_err(|source| SpecErr::Io {
        path: file,
        source: source.into(),
    })?;
    let root = dir.canonicalize().map_err(|source| SpecErr::Io {
        path: dir.into(),
        source: source.into(),
    })?;
    if !file.starts_with(root) || !file.is_file() {
        return Err(SpecErr::Path {
            path: path.into(),
            reason: "must be a file inside this workflow directory",
        });
    }
    Ok(file)
}

pub(super) fn resolve_argv(dir: &Path, run: &str) -> Result<Vec<OsString>, SpecErr> {
    let words = shlex::split(run)
        .filter(|words| !words.is_empty())
        .ok_or(SpecErr::Command)?;
    resolve_words(dir, &words)
}

/// A split `run` command's argv, each word with a `/` confined to the definition directory.
pub(super) fn resolve_words(dir: &Path, words: &[String]) -> Result<Vec<OsString>, SpecErr> {
    words
        .iter()
        .map(|word| {
            if word.contains('/') {
                Ok(confined_file(dir, word)?.into_os_string())
            } else {
                Ok(word.into())
            }
        })
        .collect()
}

pub(super) fn parse_deadline(raw: &str) -> Result<jiff::Timestamp, jiff::Error> {
    if let Ok(at) = raw.parse::<jiff::Timestamp>() {
        return Ok(at);
    }
    let civil = raw.parse::<jiff::civil::DateTime>().or_else(|_| {
        raw.parse::<jiff::civil::Date>()
            .map(|date| date.at(0, 0, 0, 0))
    })?;
    Ok(civil.to_zoned(jiff::tz::TimeZone::system())?.timestamp())
}

pub(super) fn parse_window(raw: &str) -> Result<crate::harness::when::WhenExpr, SpecErr> {
    let mut expression = String::new();
    for part in raw.split_inclusive(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_') {
        let word =
            part.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_');
        if matches!(word, "5h.left" | "7d.left") {
            expression.push_str("window.");
        }
        expression.push_str(part);
    }
    let expr = crate::harness::when::WhenExpr::parse_with_stages(&[expression], None)?;
    if !expr.terms().all(|term| term.key.starts_with("window.")) {
        return Err(SpecErr::WindowKeys);
    }
    Ok(expr)
}
