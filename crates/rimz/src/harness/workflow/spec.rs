use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::harness::schedule::{self, signal::SignalSelector, when::WhenExpr};
use crate::store::workflow::{Key, RouteDefaults, RunRecord, StepLifecycle, StepVisits};

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
    pub route_defaults: RouteDefaults,
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
    pub(super) fn is_break(&self, node: &str) -> bool {
        node == "@break" && self.route_defaults == RouteDefaults::OnBreak
    }

    /// `Some(empty)` demands checkout facts; `None` means no condition or wake needs them.
    pub(crate) fn forge_branches(&self) -> Result<Option<BTreeSet<String>>, SpecErr> {
        let mut branches = None;
        for check in self.checks.values() {
            if !matches!(check.kind(), Some(CheckKind::Condition | CheckKind::Sleep)) {
                for wake in check.wake.iter().flatten() {
                    let (selector, matches) = wake.subscription()?;
                    if schedule::arm::default_signal_match_key(&selector, &matches) == Some("path")
                    {
                        branches.get_or_insert_with(BTreeSet::new);
                    }
                }
                continue;
            }
            for case in check.condition_cases().iter() {
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
                #[serde(rename_all = "kebab-case")]
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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub struct Limit {
    #[schemars(range(min = 1))]
    pub max_parallel: Option<u32>,
    pub stagger: Option<String>,
}

pub(super) fn duration(raw: &str) -> Result<std::time::Duration, crate::utils::time::TimeInputErr> {
    use crate::utils::time::{
        DurationUnit, TimeInputErr, format_duration_compact, parse_duration_units,
    };
    let duration = parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
            DurationUnit::Day,
        ],
    )?;
    let maximum = std::time::Duration::from_secs(365 * 86_400);
    if duration > maximum {
        return Err(TimeInputErr::DurationTooLong {
            value: raw.to_owned(),
            maximum: format_duration_compact(maximum),
        });
    }
    Ok(duration)
}

impl Limit {
    pub(in crate::harness) fn stagger(
        &self,
    ) -> Result<Option<std::time::Duration>, crate::utils::time::TimeInputErr> {
        self.stagger.as_deref().map(duration).transpose()
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
    pub invalid: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub struct Check {
    pub summary: Option<String>,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub agent: Option<String>,
    pub sleep: Option<String>,
    pub when: Option<Vec<String>>,
    #[serde(rename = "for")]
    pub hold: Option<String>,
    pub then: Option<String>,
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
    pub on: BTreeMap<String, Route>,
    pub timeout: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(deny_unknown_fields)]
pub enum Route {
    Target(String),
    Delayed { to: String, delay: Option<String> },
}

impl<'de> Deserialize<'de> for Route {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Route;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a route target or a table with to and optional delay")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Route, E> {
                Ok(Route::Target(value.into()))
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<Route, M::Error> {
                #[derive(Deserialize)]
                struct Delayed {
                    to: String,
                    delay: Option<String>,
                }
                let route =
                    Delayed::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(Route::Delayed {
                    to: route.to,
                    delay: route.delay,
                })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl From<&str> for Route {
    fn from(target: &str) -> Self {
        Self::Target(target.into())
    }
}

impl Route {
    pub(super) fn target(&self) -> &str {
        match self {
            Self::Target(target) | Self::Delayed { to: target, .. } => target,
        }
    }

    pub(super) fn delay(&self) -> Option<&str> {
        match self {
            Self::Target(_) => None,
            Self::Delayed { delay, .. } => delay.as_deref(),
        }
    }
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
    Sleep,
}

impl Check {
    pub fn subscriptions(&self) -> Result<Vec<Subscription>, SpecErr> {
        if !matches!(self.kind(), Some(CheckKind::Condition | CheckKind::Sleep)) {
            return self.wake.iter().flatten().map(Wake::subscription).collect();
        }
        let mut subscriptions = Vec::new();
        for case in self.condition_cases().iter() {
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
        match (
            &self.run,
            &self.shell,
            &self.agent,
            &self.sleep,
            self.cases.is_empty(),
        ) {
            (Some(run), None, None, None, true) => Some(CheckKind::Run(run)),
            (None, Some(shell), None, None, true) => Some(CheckKind::Shell(shell)),
            (None, None, Some(agent), None, true) => Some(CheckKind::Agent(agent)),
            (None, None, None, None, false) => Some(CheckKind::Condition),
            (None, None, None, Some(_), true) => Some(CheckKind::Sleep),
            _ => None,
        }
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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub struct Step {
    pub summary: Option<String>,
    pub agent: Option<String>,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub lifecycle: Option<crate::store::workflow::StepLifecycle>,
    pub keep: Option<bool>,
    pub notes: Option<bool>,
    pub prompt_file: Option<String>,
    pub window: Option<String>,
    pub takeover: Option<bool>,
    pub share: Option<bool>,
    #[schemars(with = "Option<String>")]
    pub throttle: Option<crate::config::ThrottleSwitch>,
    #[schemars(with = "Option<String>")]
    pub capacity_wait: Option<crate::config::CapacityWait>,
    pub limit: Option<Limit>,
    #[serde(default, deserialize_with = "then_checks")]
    #[schemars(required, with = "ResetOn", !default)]
    pub then: Vec<String>,
    #[serde(default)]
    pub on: StepEdges,
    #[schemars(range(min = 1))]
    pub max_visits: Option<u64>,
    pub max_visits_per_outcome: Option<OutcomeCap>,
    pub timeout: Option<String>,
}

#[derive(Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub enum OutcomeCap {
    Count(#[schemars(range(min = 1))] u64),
    Rule {
        #[schemars(range(min = 1))]
        limit: u64,
        #[schemars(with = "Option<ResetOn>")]
        #[serde(rename = "reset-on")]
        reset_on: Option<Vec<String>>,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum ResetOn {
    One(String),
    Many(#[schemars(length(min = 1))] Vec<String>),
}

fn then_checks<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    ResetOn::deserialize(deserializer).map(|checks| match checks {
        ResetOn::One(check) => vec![check],
        ResetOn::Many(checks) => checks,
    })
}

impl<'de> Deserialize<'de> for OutcomeCap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OutcomeCap;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a positive visit limit or a table with limit and optional reset-on")
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(OutcomeCap::Count(value))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                self.visit_u64(u64::try_from(value).map_err(E::custom)?)
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Self::Value, M::Error> {
                #[derive(Deserialize)]
                #[serde(rename_all = "kebab-case")]
                struct Rule {
                    limit: u64,
                    reset_on: Option<ResetOn>,
                }
                let rule = Rule::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                let reset_on = match rule.reset_on {
                    None => None,
                    Some(ResetOn::One(outcome)) => Some(vec![outcome]),
                    Some(ResetOn::Many(outcomes)) if outcomes.is_empty() => {
                        return Err(serde::de::Error::custom("reset-on must be nonempty"));
                    }
                    Some(ResetOn::Many(outcomes)) => Some(outcomes),
                };
                Ok(OutcomeCap::Rule {
                    limit: rule.limit,
                    reset_on,
                })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl OutcomeCap {
    pub fn limit(&self) -> u64 {
        match self {
            Self::Count(limit) | Self::Rule { limit, .. } => *limit,
        }
    }

    pub fn reset_on(&self) -> &[String] {
        match self {
            Self::Count(_) => &[],
            Self::Rule { reset_on, .. } => reset_on.as_deref().unwrap_or_default(),
        }
    }
}

impl Display for OutcomeCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.limit())?;
        if !self.reset_on().is_empty() {
            write!(f, ", reset-on {}", self.reset_on().join(", "))?;
        }
        Ok(())
    }
}

pub enum StepKind<'a> {
    Agent(&'a str),
    Run(&'a str),
    Shell(&'a str),
}

impl Step {
    pub fn lifecycle(&self) -> StepLifecycle {
        self.lifecycle.unwrap_or(match self.kind() {
            Some(StepKind::Agent(_)) => StepLifecycle::Until,
            _ => StepLifecycle::Turn,
        })
    }

    /// Configured bounds as (name, used, limit including grant), total before per-outcome.
    pub fn visit_bounds(
        &self,
        visits: Option<&StepVisits>,
        bucket: (Option<&str>, Option<&Key>),
        grant: u64,
    ) -> impl Iterator<Item = (&'static str, u64, u64)> {
        [
            ("max-visits", visits.map_or(0, |v| v.total), self.max_visits),
            (
                "max-visits-per-outcome",
                visits.map_or(0, |v| v.for_bucket(bucket)),
                self.max_visits_per_outcome.as_ref().map(OutcomeCap::limit),
            ),
        ]
        .into_iter()
        .filter_map(move |(bound, used, max)| Some((bound, used, max?.saturating_add(grant))))
    }

    /// The first exhausted bound for the record's current step and bucket.
    pub fn tripped_visit_bound(&self, record: &RunRecord) -> Option<(&'static str, u64, u64)> {
        self.visit_bounds(
            record.visits.get(&record.node),
            record.visit_bucket(),
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
    #[error(
        "{path}: {target} is not a route target; route to a node, @done, or @failed, \
         or set [workflow] on-break"
    )]
    ReservedTarget { path: String, target: String },
    #[error("{path}: @break is reserved for workflow.on-break; choose another node name")]
    ReservedNode { path: String },
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

pub fn load_run(dir: &Path, record: &RunRecord) -> Result<WorkflowSpec, SpecErr> {
    let path = crate::disk::paths::workflow_spec_file(dir);
    let text = std::fs::read_to_string(&path).map_err(|source| SpecErr::Io { path, source })?;
    parse_run(&text, record)
}

pub(crate) fn parse_run(text: &str, record: &RunRecord) -> Result<WorkflowSpec, SpecErr> {
    parse_with_defaults(text, record.default_lifecycle, record.route_defaults)
}

pub(super) fn parse(text: &str) -> Result<WorkflowSpec, SpecErr> {
    parse_with_defaults(text, StepLifecycle::Until, RouteDefaults::OnBreak)
}

fn parse_with_defaults(
    text: &str,
    default: StepLifecycle,
    routes: RouteDefaults,
) -> Result<WorkflowSpec, SpecErr> {
    let value: toml::Value = toml::from_str(text).map_err(|source| SpecErr::Toml {
        path: "workflow".into(),
        source,
    })?;
    if routes == RouteDefaults::OnBreak {
        reject_reserved_targets(&value)?;
    }
    let mut spec = tolerant::parse(value, default, routes)?;
    if routes == RouteDefaults::Park {
        return Ok(spec);
    }
    for (kind, exists) in [
        ("checks", spec.checks.contains_key("@break")),
        ("steps", spec.steps.contains_key("@break")),
    ] {
        if exists {
            return Err(SpecErr::ReservedNode {
                path: format!("{kind}.@break"),
            });
        }
    }
    if let Some(OnBreak::Agent { agent, prompt_file }) = &spec.workflow.on_break {
        spec.steps.insert(
            "@break".into(),
            Step {
                summary: Some("Investigate the workflow failure with the user".into()),
                agent: Some(agent.clone()),
                prompt_file: prompt_file.clone(),
                lifecycle: Some(StepLifecycle::Stay),
                then: vec!["@failed".into()],
                on: StepEdges {
                    invalid: Some("@failed".into()),
                    ..Default::default()
                },
                run: None,
                shell: None,
                keep: None,
                notes: None,
                window: None,
                takeover: None,
                share: None,
                throttle: None,
                capacity_wait: None,
                limit: None,
                max_visits: None,
                max_visits_per_outcome: None,
                timeout: None,
            },
        );
    }
    Ok(spec)
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
    for kind in ["checks", "steps"] {
        let Some(nodes) = value.get(kind).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, node) in nodes {
            let path = format!("{kind}.{name}");
            if let Some(then) = node.get("then") {
                reject_reserved_target(&format!("{path}.then"), then.as_str())?;
                if kind == "steps" {
                    for target in then.as_array().into_iter().flatten() {
                        reject_reserved_target(&format!("{path}.then"), target.as_str())?;
                    }
                }
            }
            let Some(on) = node.get("on").and_then(toml::Value::as_table) else {
                continue;
            };
            for (outcome, route) in on {
                if kind == "steps"
                    && !matches!(outcome.as_str(), "asked" | "cap" | "capacity" | "invalid")
                {
                    continue;
                }
                reject_reserved_target(
                    &format!("{path}.on.{outcome}"),
                    route
                        .as_str()
                        .or_else(|| route.get("to").and_then(toml::Value::as_str)),
                )?;
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
