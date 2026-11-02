use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::harness::schedule::{self, signal::SignalSelector, when::WhenExpr};
use crate::store::workflow::{Key, RunRecord, StepVisits};

pub use crate::store::workflow::ScopeKind;

type Subscription = (SignalSelector, BTreeMap<String, String>);

mod tolerant;

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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct WorkflowSpec {
    #[serde(skip)]
    #[schemars(skip)]
    pub ignored: Vec<Issue>,
    pub workflow: Header,
    #[serde(default)]
    pub checks: BTreeMap<String, Check>,
    #[serde(default)]
    pub steps: BTreeMap<String, Step>,
}

impl WorkflowSpec {
    /// `Some(empty)` demands checkout facts; `None` means no condition or wake needs them.
    pub(crate) fn forge_branches(&self) -> Result<Option<BTreeSet<String>>, SpecErr> {
        let mut branches = None;
        for check in self.checks.values() {
            if !matches!(check.kind(), Some(CheckKind::Condition)) {
                for wake in check.wake.iter().flatten() {
                    let (selector, matches) = wake.subscription()?;
                    if schedule::arm::default_signal_match_key(&selector, &matches) == Some("path")
                    {
                        branches.get_or_insert_with(BTreeSet::new);
                    }
                }
                continue;
            }
            for case in &check.cases {
                let expression = WhenExpr::parse(&case.when)?;
                if expression.reads_forge() {
                    branches
                        .get_or_insert_with(BTreeSet::new)
                        .extend(expression.forge_branches().map(str::to_owned));
                }
            }
        }
        Ok(branches)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct Header {
    pub summary: Option<String>,
    pub scope: ScopeKind,
    pub start: String,
    pub deadline: Option<String>,
    pub limit: Option<Limit>,
    #[serde(default)]
    pub on: HeaderEdges,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub struct Limit {
    #[schemars(range(min = 1))]
    pub max_active: Option<u32>,
    pub pace: Option<String>,
}

pub(super) fn duration(raw: &str) -> Result<std::time::Duration, crate::utils::time::TimeInputErr> {
    use crate::utils::time::{DurationUnit, parse_duration_units};
    parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
            DurationUnit::Day,
        ],
    )
}

impl Limit {
    pub(in crate::harness) fn pace(
        &self,
    ) -> Result<Option<std::time::Duration>, crate::utils::time::TimeInputErr> {
        self.pace.as_deref().map(duration).transpose()
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct HeaderEdges {
    pub deadline: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct StepEdges {
    pub asked: Option<String>,
    pub cap: Option<String>,
    pub capacity: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub struct Check {
    pub summary: Option<String>,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub agent: Option<String>,
    pub prompt_file: Option<String>,
    pub window: Option<String>,
    pub(super) wake: Option<Vec<Wake>>,
    #[serde(default, deserialize_with = "nonempty_cases")]
    #[schemars(length(min = 1))]
    pub cases: Vec<Case>,
    #[serde(rename = "else")]
    pub else_outcome: Option<String>,
    #[schemars(with = "Option<String>")]
    pub provider: Option<crate::ids::AgentKind>,
    #[schemars(with = "Option<String>")]
    pub account: Option<crate::ids::LoginName>,
    #[serde(default)]
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
                struct Binding {
                    signal: String,
                    #[serde(default, rename = "match")]
                    matches: BTreeMap<String, String>,
                }
                let binding =
                    Binding::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(Wake::Binding {
                    signal: binding.signal,
                    matches: binding.matches,
                })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl Wake {
    pub(super) fn subscription(&self) -> Result<Subscription, SpecErr> {
        let (raw, matches) = match self {
            Self::Selector(signal) => (signal, BTreeMap::new()),
            Self::Binding { signal, matches } => (signal, matches.clone()),
        };
        let selector = schedule::parse_signal_selector("workflow wake", raw, Some(&matches))?;
        if selector.family() == "agent" || (selector.family() == "team" && raw != "team.stage") {
            return Err(SpecErr::LifecycleWake);
        }
        Ok((selector, matches))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct Case {
    pub when: Vec<String>,
    pub outcome: String,
    #[serde(rename = "for")]
    pub hold: Option<String>,
    pub key: Option<crate::store::workflow::Key>,
}

fn nonempty_cases<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Case>, D::Error> {
    let cases = Vec::<Case>::deserialize(deserializer)?;
    if cases.is_empty() {
        return Err(serde::de::Error::custom("cases must be nonempty"));
    }
    Ok(cases)
}

pub enum CheckKind<'a> {
    Run(&'a str),
    Shell(&'a str),
    Agent(&'a str),
    Condition,
}

impl Check {
    pub fn subscriptions(&self) -> Result<Vec<Subscription>, SpecErr> {
        if !matches!(self.kind(), Some(CheckKind::Condition)) {
            return self.wake.iter().flatten().map(Wake::subscription).collect();
        }
        let mut subscriptions = Vec::new();
        for case in &self.cases {
            let expression = WhenExpr::parse(&case.when)?;
            subscriptions.extend(
                expression
                    .terms()
                    .filter_map(|term| term.wake_selector())
                    .map(|selector| (selector, BTreeMap::new())),
            );
        }
        Ok(subscriptions)
    }

    pub fn kind(&self) -> Option<CheckKind<'_>> {
        match (&self.run, &self.shell, &self.agent, self.cases.is_empty()) {
            (Some(run), None, None, true) => Some(CheckKind::Run(run)),
            (None, Some(shell), None, true) => Some(CheckKind::Shell(shell)),
            (None, None, Some(agent), true) => Some(CheckKind::Agent(agent)),
            (None, None, None, false) => Some(CheckKind::Condition),
            _ => None,
        }
    }
}

pub(super) const RESERVED_OUTCOMES: [&str; 7] = [
    "timeout", "invalid", "cap", "capacity", "deadline", "unknown", "hold",
];

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub struct Step {
    pub summary: Option<String>,
    pub agent: Option<String>,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub lifecycle: Option<crate::store::workflow::StepLifecycle>,
    pub prompt_file: Option<String>,
    pub window: Option<String>,
    pub takeover: Option<bool>,
    #[schemars(with = "Option<String>")]
    pub throttle: Option<crate::config::ThrottleSwitch>,
    #[schemars(with = "Option<String>")]
    pub capacity_wait: Option<crate::config::CapacityWait>,
    pub limit: Option<Limit>,
    #[schemars(required)]
    pub then: Option<String>,
    #[serde(default)]
    pub on: StepEdges,
    #[schemars(range(min = 1))]
    pub max_visits: Option<u64>,
    #[schemars(range(min = 1))]
    pub max_visits_per_key: Option<u64>,
    pub timeout: Option<String>,
}

pub enum StepKind<'a> {
    Agent(&'a str),
    Run(&'a str),
    Shell(&'a str),
}

impl Step {
    /// Configured bounds as (name, used, limit including grant), total before per-key.
    pub fn visit_bounds(
        &self,
        visits: Option<&StepVisits>,
        key: Option<&Key>,
        grant: u64,
    ) -> impl Iterator<Item = (&'static str, u64, u64)> {
        [
            ("max-visits", visits.map_or(0, |v| v.total), self.max_visits),
            (
                "max-visits-per-key",
                visits.map_or(0, |v| v.for_key(key)),
                self.max_visits_per_key,
            ),
        ]
        .into_iter()
        .filter_map(move |(bound, used, max)| Some((bound, used, max?.saturating_add(grant))))
    }

    /// The first exhausted bound for the record's current step and key.
    pub fn tripped_visit_bound(&self, record: &RunRecord) -> Option<(&'static str, u64, u64)> {
        self.visit_bounds(
            record.visits.get(&record.node),
            record.key.as_ref(),
            record.visit_grant,
        )
        .find(|(_, used, max)| used >= max)
    }

    pub fn kind(&self) -> Option<StepKind<'_>> {
        match (&self.agent, &self.run, &self.shell) {
            (Some(agent), None, None) => Some(StepKind::Agent(agent)),
            (None, Some(run), None) => Some(StepKind::Run(run)),
            (None, None, Some(shell)) => Some(StepKind::Shell(shell)),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpecErr {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
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
    Window(#[from] crate::harness::schedule::when::WhenError),
    #[error("only provider-window keys are allowed")]
    WindowKeys,
    #[error(transparent)]
    Signal(#[from] schedule::ScheduleErr),
    #[error("lifecycle signals cannot wake workflows; use team.stage or a non-lifecycle selector")]
    LifecycleWake,
}

pub fn load(dir: &Path) -> Result<WorkflowSpec, SpecErr> {
    let path = crate::disk::paths::workflow_spec_file(dir);
    let text = std::fs::read_to_string(&path).map_err(|source| SpecErr::Io { path, source })?;
    parse(&text)
}

pub(super) fn parse(text: &str) -> Result<WorkflowSpec, SpecErr> {
    let value: toml::Value = toml::from_str(text).map_err(|source| SpecErr::Toml {
        path: "workflow".into(),
        source,
    })?;
    tolerant::parse(value)
}

pub fn schema() -> schemars::Schema {
    schemars::schema_for!(WorkflowSpec)
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
    let file = file
        .canonicalize()
        .map_err(|source| SpecErr::Io { path: file, source })?;
    let root = dir.canonicalize().map_err(|source| SpecErr::Io {
        path: dir.into(),
        source,
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
    words
        .into_iter()
        .map(|word| {
            if word.contains('/') {
                Ok(confined_file(dir, &word)?.into_os_string())
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

pub(super) fn parse_window(raw: &str) -> Result<crate::harness::schedule::when::WhenExpr, SpecErr> {
    let mut expression = String::new();
    for part in raw.split_inclusive(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_') {
        let word =
            part.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_');
        if matches!(word, "5h.left" | "7d.left") {
            expression.push_str("window.");
        }
        expression.push_str(part);
    }
    let expr = crate::harness::schedule::when::WhenExpr::parse_with_stages(&[expression], None)?;
    if !expr.terms().all(|term| term.key.starts_with("window.")) {
        return Err(SpecErr::WindowKeys);
    }
    Ok(expr)
}
