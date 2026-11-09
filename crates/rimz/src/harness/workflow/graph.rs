use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::spec::{Check, CheckKind, OnBreak, Step, StepKind, WorkflowSpec};
use crate::config::CapacityWait;
use crate::harness::budget::BudgetSpec;
use crate::store::workflow::RouteDefaults;

const CHECK_TRIGGERS: [&str; 4] = ["unknown", "timeout", "invalid", "deadline"];
const STEP_TRIGGERS: [&str; 8] = [
    "then",
    "failed",
    "undelivered",
    "asked",
    "invalid",
    "cap",
    "capacity",
    "deadline",
];
const FAILURE_EDGES: [&str; 5] = ["timeout", "invalid", "undelivered", "cap", "deadline"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeSource {
    Explicit,
    Default,
    #[serde(rename = "on-break")]
    OnBreak,
}

impl std::fmt::Display for EdgeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Explicit => "explicit",
            Self::Default => "default",
            Self::OnBreak => "on-break",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fireability {
    Yes,
    Never,
    WithDeadline,
    InLoopRuns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NeverReason {
    NoTimeout,
    NoMaxVisits,
    CapacityWaitForever,
    JudgeRetries,
}

impl NeverReason {
    fn mark(self) -> &'static str {
        match self {
            Self::NoTimeout => "never: no timeout",
            Self::NoMaxVisits => "never: no max-visits",
            Self::CapacityWaitForever => "never: capacity-wait is forever",
            Self::JudgeRetries => "never: a judge retries once, then parks at @user",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Edge {
    pub outcome: String,
    pub target: String,
    pub source: EdgeSource,
    pub fires: Fireability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<NeverReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
}

impl Edge {
    /// The shared human mark; an unconditional edge needs none.
    pub fn mark(&self) -> Option<&'static str> {
        match self.fires {
            Fireability::Yes => None,
            Fireability::Never => self.because.map(NeverReason::mark),
            Fireability::WithDeadline => Some("only with a deadline"),
            Fireability::InLoopRuns => Some("only when a loop task starts the run"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Walk {
    pub nodes: Vec<WalkNode>,
    pub unreachable: Vec<String>,
}

#[derive(Debug)]
pub struct WalkNode {
    pub node: String,
    pub hangs_under: Option<String>,
}

impl WorkflowSpec {
    /// What one run reserves against a daily cap: its agent steps' per-launch budgets, summed.
    pub(in crate::harness) fn reserved_usd(&self) -> f64 {
        self.steps
            .values()
            .filter(|step| matches!(step.kind(), Some(StepKind::Agent(_))))
            .filter_map(|step| step.budget.as_deref()?.parse::<BudgetSpec>().ok())
            .fold(0.0, |total, budget| total + budget.cap_usd)
    }

    /// The profile of every agent step and agent check, beside the account a step pins; a
    /// judge runs on its profile's own login.
    pub(in crate::harness) fn agent_launches(&self) -> Vec<(&str, Option<&crate::ids::LoginName>)> {
        let steps = self.steps.values().filter_map(|step| match step.kind() {
            Some(StepKind::Agent(agent)) => Some((agent, step.account.as_ref())),
            _ => None,
        });
        let checks = self.checks.values().filter_map(|check| match check.kind() {
            Some(CheckKind::Agent(agent)) => Some((agent, None)),
            _ => None,
        });
        steps.chain(checks).collect()
    }

    /// Reverse postorder, with returning steps below their earlier sending check.
    pub fn walk(&self) -> Walk {
        let mut seen = BTreeSet::new();
        let mut order = self.walk_component(&self.workflow.start, &mut seen);
        if self.workflow.deadline.is_some()
            && let Some(target) = self.target(&self.workflow.start, "deadline")
        {
            order.extend(self.walk_component(&target, &mut seen));
        } else if let Some(target) = &self.workflow.on.deadline {
            order.extend(self.walk_component(target, &mut seen));
        }
        let parents = self.hanging_steps(&order);
        let mut nodes = Vec::new();
        let mut placed = BTreeSet::new();
        for node in order {
            if !placed.insert(node.clone()) {
                continue;
            }
            nodes.push(WalkNode {
                node: node.clone(),
                hangs_under: None,
            });
            for edge in self.edges(&node) {
                if parents.get(&edge.target) == Some(&node) && placed.insert(edge.target.clone()) {
                    nodes.push(WalkNode {
                        node: edge.target,
                        hangs_under: Some(node.clone()),
                    });
                }
            }
        }
        let unreachable = self
            .checks
            .keys()
            .chain(self.steps.keys())
            .filter(|node| !seen.contains(*node))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Walk { nodes, unreachable }
    }

    fn walk_component(&self, start: &str, seen: &mut BTreeSet<String>) -> Vec<String> {
        let mut pending = vec![(start.to_owned(), false)];
        let mut order = Vec::new();
        while let Some((node, finished)) = pending.pop() {
            if finished {
                order.push(node);
                continue;
            }
            if !(self.checks.contains_key(&node) || self.steps.contains_key(&node))
                || !seen.insert(node.clone())
            {
                continue;
            }
            pending.push((node.clone(), true));
            for edge in self.edges(&node) {
                if edge.outcome != "deadline" && edge.fires != Fireability::Never {
                    pending.push((edge.target, false));
                }
            }
        }
        order.reverse();
        order
    }

    fn hanging_steps(&self, order: &[String]) -> BTreeMap<String, String> {
        let positions = order
            .iter()
            .enumerate()
            .map(|(index, node)| (node, index))
            .collect::<BTreeMap<_, _>>();
        self.steps
            .iter()
            .filter_map(|(node, step)| {
                let parent = step.then.iter().find(|parent| {
                    self.checks.contains_key(*parent)
                        && positions
                            .get(*parent)
                            .zip(positions.get(node))
                            .is_some_and(|(parent, step)| parent < step)
                        && self.edges(parent).iter().any(|edge| {
                            edge.outcome != "deadline"
                                && edge.target == *node
                                && edge.fires != Fireability::Never
                        })
                })?;
                Some((node.clone(), parent.clone()))
            })
            .collect()
    }

    /// Node edges in display order, including conditional and never-firing routes.
    pub fn edges(&self, node: &str) -> Vec<Edge> {
        self.resolved_edges(node)
            .unwrap_or_default()
            .into_iter()
            .filter(|edge| {
                self.steps
                    .get(node)
                    .is_none_or(|step| match edge.outcome.as_str() {
                        "asked" => {
                            matches!(step.kind(), Some(StepKind::Agent(_)))
                                && step.lifecycle().is_turn()
                        }
                        "capacity" => matches!(step.kind(), Some(StepKind::Agent(_))),
                        _ => true,
                    })
            })
            .collect()
    }

    /// Resolve a routing decision against this spec, including engine-owned outcomes.
    pub fn target(&self, node: &str, outcome: &str) -> Option<String> {
        let edges = self.resolved_edges(node)?;
        if self.checks.contains_key(node) {
            return if outcome == "hold" {
                Some("@wait".into())
            } else {
                edges
                    .into_iter()
                    .find(|edge| edge.outcome == outcome)
                    .map(|edge| edge.target)
            };
        }
        let step = self.steps.get(node);
        let message = step.is_some_and(|step| matches!(step.kind(), Some(StepKind::Message(_))));
        let outcome = match outcome {
            "died" if message => "undelivered",
            _ if self.routes_on_failed(node, outcome) => "failed",
            "ended" | "died" | "canceled" | "limited" | "timeout" => "then",
            other => other,
        };
        edges
            .into_iter()
            .find(|edge| edge.outcome == outcome)
            .map(|edge| edge.target)
            .or_else(|| {
                if self.route_defaults == RouteDefaults::Park {
                    matches!(outcome, "then" | "asked").then(|| "@user".into())
                } else {
                    (outcome == "then").then(|| "@failed".into())
                }
            })
    }

    /// Whether a step's attempt ending leaves through its `on.failed`, for settlement and
    /// inspection alike.
    pub(super) fn routes_on_failed(&self, node: &str, ending: &str) -> bool {
        matches!(ending, "died" | "timeout" | "limited")
            && self
                .steps
                .get(node)
                .is_some_and(|step| step.on.failed.is_some())
    }

    pub(super) fn engine_target(&self, node: &str, outcome: &str) -> String {
        // Engine callers have a current node and an engine-owned or normalized outcome.
        self.target(node, outcome).expect("admitted workflow route")
    }

    pub(super) fn resolved_edges(&self, node: &str) -> Option<Vec<Edge>> {
        let mut edges = self.node_edges(node)?;
        let outcomes = if let Some(check) = self.checks.get(node) {
            if check.is_sleep() {
                ["then", "sleeping", "unknown", "invalid", "deadline"]
                    .map(str::to_owned)
                    .to_vec()
            } else {
                check
                    .cases
                    .iter()
                    .map(|case| case.outcome.clone())
                    .chain(check.else_outcome.iter().cloned())
                    .chain(
                        check
                            .on
                            .keys()
                            .filter(|outcome| !CHECK_TRIGGERS.contains(&outcome.as_str()))
                            .cloned(),
                    )
                    .chain(CHECK_TRIGGERS.map(str::to_owned))
                    .collect::<Vec<_>>()
            }
        } else {
            STEP_TRIGGERS.map(str::to_owned).to_vec()
        };
        Some(
            outcomes
                .into_iter()
                .filter_map(|outcome| {
                    let (target, explicit) = edges.remove(&outcome)?;
                    let (fires, because) = self.fireability(node, &outcome);
                    Some(Edge {
                        delay: self
                            .checks
                            .get(node)
                            .and_then(|check| check.on.get(&outcome))
                            .and_then(|route| route.delay())
                            .map(str::to_owned),
                        target,
                        source: if explicit && !self.declared_default(node, &outcome) {
                            EdgeSource::Explicit
                        } else if explicit {
                            EdgeSource::Default
                        } else if !self.is_break(node)
                            && self.route_defaults == RouteDefaults::OnBreak
                            && self.workflow.on_break.is_some()
                            && FAILURE_EDGES.contains(&outcome.as_str())
                        {
                            EdgeSource::OnBreak
                        } else {
                            EdgeSource::Default
                        },
                        outcome,
                        fires,
                        because,
                    })
                })
                .collect(),
        )
    }

    /// Whether the check's route for `outcome` is written and marked `default = true`.
    pub(super) fn declared_default(&self, node: &str, outcome: &str) -> bool {
        self.checks
            .get(node)
            .and_then(|check| check.on.get(outcome))
            .is_some_and(super::spec::Route::is_default)
    }

    fn fireability(&self, node: &str, outcome: &str) -> (Fireability, Option<NeverReason>) {
        if outcome == "deadline" {
            return (
                if self.workflow.deadline.is_some() {
                    Fireability::Yes
                } else {
                    Fireability::WithDeadline
                },
                None,
            );
        }
        if let Some(check) = self.checks.get(node) {
            let because = match outcome {
                "timeout" if check.timeout.is_none() => Some(NeverReason::NoTimeout),
                "invalid"
                    if self.route_defaults == RouteDefaults::Park
                        && matches!(check.kind(), Some(CheckKind::Agent(_))) =>
                {
                    Some(NeverReason::JudgeRetries)
                }
                _ => None,
            };
            return (
                if because.is_some() {
                    Fireability::Never
                } else {
                    Fireability::Yes
                },
                because,
            );
        }
        let Some(step) = self.steps.get(node) else {
            return (Fireability::Yes, None);
        };
        if outcome == "cap" && step.max_visits.is_none() && step.max_visits_per_outcome.is_none() {
            return (Fireability::Never, Some(NeverReason::NoMaxVisits));
        }
        if outcome != "capacity" {
            return (Fireability::Yes, None);
        }
        if !matches!(step.capacity_wait, Some(CapacityWait::Duration(_))) {
            return (Fireability::Never, Some(NeverReason::CapacityWaitForever));
        }
        (Fireability::InLoopRuns, None)
    }

    fn node_edges(&self, node: &str) -> Option<BTreeMap<String, (String, bool)>> {
        let mut edges = if let Some(check) = self.checks.get(node) {
            check
                .edges()
                .into_iter()
                .map(|(outcome, target)| {
                    let route = check.on.get(&outcome).filter(|_| {
                        !check.is_sleep()
                            || (outcome == "invalid"
                                && self.route_defaults == RouteDefaults::OnBreak)
                    });
                    let explicit = route.is_some() || (outcome == "then" && check.is_sleep());
                    let target = route.map_or(target, |route| route.target().to_owned());
                    (outcome, (target, explicit))
                })
                .collect()
        } else {
            self.steps
                .get(node)?
                .edges()
                .into_iter()
                .map(|(outcome, (target, explicit))| {
                    (outcome.to_owned(), (target.to_owned(), explicit))
                })
                .collect::<BTreeMap<_, _>>()
        };
        if let Some(step) = self.steps.get(node) {
            let failure = match step.kind() {
                Some(StepKind::Agent(_)) if self.route_defaults == RouteDefaults::OnBreak => {
                    Some(("invalid", &step.on.invalid))
                }
                Some(StepKind::Message(_)) => Some(("undelivered", &step.on.undelivered)),
                _ => None,
            };
            if let Some((outcome, route)) = failure {
                let fallback = match self.route_defaults {
                    RouteDefaults::OnBreak => String::new(),
                    RouteDefaults::Park => "@user".into(),
                };
                edges.insert(
                    outcome.into(),
                    (route.clone().unwrap_or(fallback), route.is_some()),
                );
            }
        }
        if !self.is_break(node) {
            edges.insert(
                "deadline".into(),
                (
                    self.workflow
                        .on
                        .deadline
                        .as_deref()
                        .unwrap_or("@user")
                        .into(),
                    self.workflow.on.deadline.is_some(),
                ),
            );
        }
        if self.route_defaults == RouteDefaults::OnBreak {
            let target = match &self.workflow.on_break {
                _ if self.is_break(node) => "@failed",
                Some(OnBreak::Target(target)) => target.as_str(),
                Some(OnBreak::Agent { .. }) => "@break",
                None => "@failed",
            };
            for (outcome, (route, explicit)) in &mut edges {
                if !*explicit && FAILURE_EDGES.contains(&outcome.as_str()) {
                    *route = target.into();
                }
            }
        }
        Some(edges)
    }
}

impl Check {
    fn edges(&self) -> BTreeMap<String, String> {
        if self.is_sleep() {
            let mut edges = BTreeMap::from([
                ("sleeping".into(), "@wait".into()),
                ("invalid".into(), "@user".into()),
            ]);
            if let Some(target) = &self.then {
                edges.insert("then".into(), target.clone());
            }
            if self.when.is_some() {
                edges.insert("unknown".into(), "@wait".into());
            }
            return edges;
        }
        let mut edges = self
            .on
            .iter()
            .map(|(outcome, route)| (outcome.clone(), route.target().to_owned()))
            .collect::<BTreeMap<_, _>>();
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

impl Step {
    fn edges(&self) -> BTreeMap<&'static str, (&str, bool)> {
        [
            ("then", self.then.first().map(String::as_str), None),
            ("failed", self.on.failed.as_deref(), None),
            ("asked", self.on.asked.as_deref(), None),
            ("cap", self.on.cap.as_deref(), Some("@user")),
            ("capacity", self.on.capacity.as_deref(), Some("@failed")),
        ]
        .into_iter()
        .filter_map(|(outcome, target, default)| {
            Some((outcome, (target.or(default)?, target.is_some())))
        })
        .collect()
    }
}
