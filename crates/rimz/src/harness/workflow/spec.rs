use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::harness::schedule::{self, signal::SignalSelector, when::WhenExpr};

pub use crate::store::workflow::ScopeKind;

type Subscription = (SignalSelector, BTreeMap<String, String>);

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    pub workflow: Header,
    #[serde(default)]
    pub checks: BTreeMap<String, Check>,
    #[serde(default)]
    pub steps: BTreeMap<String, Step>,
}

impl WorkflowSpec {
    /// `Some(empty)` demands checkout facts; `None` means no case reads the forge.
    pub(crate) fn forge_branches(&self) -> Result<Option<BTreeSet<String>>, SpecErr> {
        let mut branches = None;
        for check in self.checks.values() {
            if !matches!(check.kind(), Some(CheckKind::Condition)) {
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
#[serde(deny_unknown_fields)]
pub struct Header {
    pub scope: ScopeKind,
    pub start: String,
    pub deadline: Option<String>,
    #[serde(default)]
    pub on: HeaderEdges,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderEdges {
    pub deadline: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepEdges {
    pub asked: Option<String>,
    pub cap: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Check {
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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum Wake {
    Selector(String),
    Binding {
        signal: String,
        #[serde(default, rename = "match")]
        matches: BTreeMap<String, String>,
    },
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
#[serde(deny_unknown_fields)]
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

pub(super) enum CheckKind<'a> {
    Run(&'a str),
    Shell(&'a str),
    Agent(&'a str),
    Condition,
}

impl Check {
    pub(super) fn subscriptions(&self) -> Result<Vec<Subscription>, SpecErr> {
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

    pub(super) fn kind(&self) -> Option<CheckKind<'_>> {
        match (&self.run, &self.shell, &self.agent, self.cases.is_empty()) {
            (Some(run), None, None, true) => Some(CheckKind::Run(run)),
            (None, Some(shell), None, true) => Some(CheckKind::Shell(shell)),
            (None, None, Some(agent), true) => Some(CheckKind::Agent(agent)),
            (None, None, None, false) => Some(CheckKind::Condition),
            _ => None,
        }
    }

    pub(super) fn edges(&self) -> BTreeMap<String, String> {
        let mut edges = self.on.clone();
        for outcome in ["timeout", "invalid"] {
            edges
                .entry(outcome.into())
                .or_insert_with(|| "@user".into());
        }
        if matches!(self.kind(), Some(CheckKind::Condition)) {
            edges
                .entry("unknown".into())
                .or_insert_with(|| "@wait".into());
        }
        edges
    }
}

pub(super) const RESERVED_OUTCOMES: [&str; 6] =
    ["timeout", "invalid", "cap", "deadline", "unknown", "hold"];

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Step {
    pub agent: String,
    pub prompt_file: String,
    pub window: Option<String>,
    #[serde(default)]
    pub takeover: bool,
    #[schemars(required)]
    pub then: Option<String>,
    #[serde(default)]
    pub on: StepEdges,
    #[schemars(range(min = 1))]
    pub max_visits: Option<u64>,
    pub timeout: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SpecErr {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
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
    let value: toml::Value = toml::from_str(&text).map_err(|source| SpecErr::Toml {
        path: "workflow".into(),
        source,
    })?;
    value.clone().try_into().map_err(|source| SpecErr::Toml {
        path: schema_path(&value),
        source,
    })
}

fn schema_path(value: &toml::Value) -> String {
    if let Some(table) = value.as_table()
        && let Some(key) = table
            .keys()
            .find(|key| !matches!(key.as_str(), "workflow" | "checks" | "steps"))
    {
        return key.clone();
    }
    for table in ["checks", "steps"] {
        let Some(nodes) = value.get(table) else {
            continue;
        };
        let Some(nodes) = nodes.as_table() else {
            return table.into();
        };
        for (name, node) in nodes {
            let invalid = match table {
                "checks" => node.clone().try_into::<Check>().is_err(),
                _ => node.clone().try_into::<Step>().is_err(),
            };
            if invalid {
                if table == "checks"
                    && let Some(wake) = node.get("wake")
                    && wake.clone().try_into::<Vec<Wake>>().is_err()
                {
                    return format!("{table}.{name}.wake");
                }
                return format!("{table}.{name}");
            }
        }
    }
    "workflow".into()
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
