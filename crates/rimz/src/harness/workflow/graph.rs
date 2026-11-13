use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::definition::{BREAK, Definition, Kind, Target};
use super::spec::{Check, OnBreak, Step};
use crate::config::CapacityWait;

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
    GiveUpForever,
}

impl NeverReason {
    fn mark(self) -> &'static str {
        match self {
            Self::NoTimeout => "never: no timeout",
            Self::NoMaxVisits => "never: no max-visits",
            Self::GiveUpForever => "never: give-up is forever",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Edge {
    pub outcome: String,
    pub target: Target,
    pub source: EdgeSource,
    pub fires: Fireability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<NeverReason>,
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

impl Definition {
    /// What one run reserves against a daily cap: its agent steps' per-launch budgets, summed.
    pub(in crate::harness) fn reserved_usd(&self) -> f64 {
        self.steps()
            .map(|(_, step)| step)
            .filter(|step| matches!(step.kind(), Some(Kind::Agent(_))))
            .filter_map(|step| step.budget()?.value().ok())
            .fold(0.0, |total, budget| total + budget.cap_usd)
    }

    /// The profile of every agent step and agent check, beside the account a step pins; a
    /// judge runs on its profile's own login.
    pub(in crate::harness) fn agent_launches(&self) -> Vec<(&str, Option<&crate::ids::LoginName>)> {
        let steps = self.steps().filter_map(|(_, step)| match step.kind() {
            Some(Kind::Agent(agent)) => Some((agent.as_str(), step.account())),
            _ => None,
        });
        let checks = self.checks().filter_map(|(_, check)| match check.kind() {
            Some(Kind::Judge(agent)) => Some((agent.as_str(), None)),
            _ => None,
        });
        steps.chain(checks).collect()
    }

    /// Reverse postorder, with returning steps below their earlier sending check.
    pub fn walk(&self) -> Walk {
        let mut seen = BTreeSet::new();
        let mut order = self.walk_component(self.start(), &mut seen);
        if self.deadline().is_some()
            && let Some(target) = self.target(self.start(), "deadline")
        {
            order.extend(self.walk_component(target.as_str(), &mut seen));
        } else if let Some(target) = self.on_deadline() {
            order.extend(self.walk_component(target.as_str(), &mut seen));
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
                let Some(target) = edge.target.node() else {
                    continue;
                };
                if parents.get(target) == Some(&node) && placed.insert(target.to_owned()) {
                    nodes.push(WalkNode {
                        node: target.to_owned(),
                        hangs_under: Some(node.clone()),
                    });
                }
            }
        }
        let unreachable = self
            .checks()
            .chain(self.steps())
            .map(|(name, _)| name)
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
            if self.node(&node).is_none() || !seen.insert(node.clone()) {
                continue;
            }
            pending.push((node.clone(), true));
            for edge in self.edges(&node) {
                if edge.outcome != "deadline"
                    && edge.fires != Fireability::Never
                    && let Some(target) = edge.target.node()
                {
                    pending.push((target.to_owned(), false));
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
        self.steps()
            .filter_map(|(node, _)| {
                let parent = self.step(node)?.then().iter().find_map(|parent| {
                    let parent = parent.node()?;
                    (self.check(parent).is_some()
                        && positions
                            .get(&parent.to_owned())
                            .zip(positions.get(node))
                            .is_some_and(|(parent, step)| parent < step)
                        && self.edges(parent).iter().any(|edge| {
                            edge.outcome != "deadline"
                                && edge.target.node() == Some(node.as_str())
                                && edge.fires != Fireability::Never
                        }))
                    .then_some(parent)
                })?;
                Some((node.clone(), parent.to_owned()))
            })
            .collect()
    }

    /// Node edges in display order, including conditional and never-firing routes.
    pub fn edges(&self, node: &str) -> Vec<Edge> {
        let step = self.step(node);
        self.resolved_edges(node)
            .unwrap_or_default()
            .iter()
            .filter(|edge| {
                step.is_none_or(|step| match edge.outcome.as_str() {
                    "asked" => {
                        matches!(step.kind(), Some(Kind::Agent(_))) && step.lifecycle().is_turn()
                    }
                    "capacity" => matches!(step.kind(), Some(Kind::Agent(_))),
                    _ => true,
                })
            })
            .cloned()
            .collect()
    }

    /// Resolve a routing decision against this definition, including engine-owned outcomes.
    pub fn target(&self, node: &str, outcome: &str) -> Option<Target> {
        let edges = self.resolved_edges(node)?;
        if self.check(node).is_some() {
            return if outcome == "hold" {
                Some(Target::Wait)
            } else if outcome == "budget" {
                Some(Target::Failed)
            } else {
                edges
                    .iter()
                    .find(|edge| edge.outcome == outcome)
                    .map(|edge| edge.target.clone())
            };
        }
        let message = matches!(self.step_kind(node), Some(Kind::Message(_)));
        let outcome = match outcome {
            "died" if message => "undelivered",
            _ if self.routes_on_failed(node, outcome) => "failed",
            "ended" | "died" | "canceled" | "limited" | "timeout" => "then",
            other => other,
        };
        edges
            .iter()
            .find(|edge| edge.outcome == outcome)
            .map(|edge| edge.target.clone())
            .or_else(|| (outcome == "then").then_some(Target::Failed))
    }

    /// Whether a step's attempt ending leaves through its `on.failed`, for settlement and
    /// inspection alike.
    pub(super) fn routes_on_failed(&self, node: &str, ending: &str) -> bool {
        matches!(ending, "died" | "timeout" | "limited")
            && self.step(node).is_some_and(|step| step.routes("failed"))
    }

    pub(super) fn engine_target(&self, node: &str, outcome: &str) -> Target {
        // Engine callers have a current node and an engine-owned or normalized outcome.
        self.target(node, outcome).expect("admitted workflow route")
    }

    /// Every edge out of `node`, resolved at compile; `None` when no node has the name.
    pub(super) fn resolved_edges(&self, node: &str) -> Option<&[Edge]> {
        self.node(node).map(super::definition::Node::resolved_edges)
    }

    pub(super) fn compile_edges(&self, node: &str) -> Option<Vec<Edge>> {
        let mut edges = self.node_edges(node)?;
        let authored = self.authored();
        let outcomes = if let Some(check) = authored.checks.get(node) {
            if self.check(node).is_some_and(|check| check.is_sleep()) {
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
                        target,
                        source: if explicit {
                            EdgeSource::Explicit
                        } else if !self.is_break(node)
                            && authored.workflow.on_break.is_some()
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

    fn fireability(&self, node: &str, outcome: &str) -> (Fireability, Option<NeverReason>) {
        let authored = self.authored();
        if outcome == "deadline" {
            return (
                if authored.workflow.deadline.is_some() {
                    Fireability::Yes
                } else {
                    Fireability::WithDeadline
                },
                None,
            );
        }
        if let Some(check) = authored.checks.get(node) {
            let because =
                (outcome == "timeout" && check.timeout.is_none()).then_some(NeverReason::NoTimeout);
            return (
                if because.is_some() {
                    Fireability::Never
                } else {
                    Fireability::Yes
                },
                because,
            );
        }
        let Some(step) = authored.steps.get(node) else {
            return (Fireability::Yes, None);
        };
        if outcome == "cap" && step.max_visits.is_none() {
            return (Fireability::Never, Some(NeverReason::NoMaxVisits));
        }
        if outcome != "capacity" {
            return (Fireability::Yes, None);
        }
        if !matches!(step.capacity_wait, Some(CapacityWait::Duration(_))) {
            return (Fireability::Never, Some(NeverReason::GiveUpForever));
        }
        (Fireability::InLoopRuns, None)
    }

    fn node_edges(&self, node: &str) -> Option<BTreeMap<String, (Target, bool)>> {
        let authored = self.authored();
        let mut edges = if let Some(check) = authored.checks.get(node) {
            let sleep = self.check(node).is_some_and(|check| check.is_sleep());
            check
                .edges(self.check_kind(node))
                .into_iter()
                .map(|(outcome, target)| {
                    let route = check
                        .on
                        .get(&outcome)
                        .filter(|_| !sleep || outcome == "invalid");
                    let explicit = route.is_some() || (outcome == "then" && sleep);
                    let target = route.map_or(target, |route| Target::parse(route));
                    (outcome, (target, explicit))
                })
                .collect()
        } else {
            authored
                .steps
                .get(node)?
                .edges()
                .into_iter()
                .map(|(outcome, edge)| (outcome.to_owned(), edge))
                .collect::<BTreeMap<_, _>>()
        };
        if let Some(step) = authored.steps.get(node) {
            let failure = match self.step_kind(node) {
                Some(Kind::Agent(_)) => Some(("invalid", &step.on.invalid)),
                Some(Kind::Message(_)) => Some(("undelivered", &step.on.undelivered)),
                _ => None,
            };
            if let Some((outcome, route)) = failure {
                edges.insert(
                    outcome.into(),
                    (
                        Target::parse(route.as_deref().unwrap_or_default()),
                        route.is_some(),
                    ),
                );
            }
        }
        if !self.is_break(node) {
            edges.insert(
                "deadline".into(),
                (
                    self.on_deadline().cloned().unwrap_or(Target::User),
                    self.on_deadline().is_some(),
                ),
            );
        }
        let target = match &authored.workflow.on_break {
            _ if self.is_break(node) => Target::Failed,
            Some(OnBreak::Target(target)) => Target::parse(target),
            Some(OnBreak::Agent { .. }) => Target::Node(BREAK.into()),
            None => Target::Failed,
        };
        for (outcome, (route, explicit)) in &mut edges {
            if !*explicit && FAILURE_EDGES.contains(&outcome.as_str()) {
                *route = target.clone();
            }
        }
        Some(edges)
    }
}

impl Check {
    fn edges(&self, kind: Option<&Kind>) -> BTreeMap<String, Target> {
        if matches!(kind, Some(Kind::Sleep(_))) {
            let mut edges = BTreeMap::from([
                ("sleeping".into(), Target::Wait),
                ("invalid".into(), Target::User),
            ]);
            if let Some(target) = &self.then {
                edges.insert("then".into(), Target::parse(target));
            }
            if self.when.is_some() {
                edges.insert("unknown".into(), Target::Wait);
            }
            return edges;
        }
        let mut edges = self
            .on
            .iter()
            .map(|(outcome, route)| (outcome.clone(), Target::parse(route)))
            .collect::<BTreeMap<_, _>>();
        for outcome in ["timeout", "invalid"] {
            edges.entry(outcome.into()).or_insert(Target::User);
        }
        edges
    }
}

impl Step {
    fn edges(&self) -> BTreeMap<&'static str, (Target, bool)> {
        [
            ("then", self.then.first(), None),
            ("failed", self.on.failed.as_ref(), None),
            ("asked", self.on.asked.as_ref(), None),
            ("cap", self.on.cap.as_ref(), Some(Target::User)),
            ("capacity", self.on.capacity.as_ref(), Some(Target::Failed)),
        ]
        .into_iter()
        .filter_map(|(outcome, target, default)| {
            let explicit = target.is_some();
            Some((
                outcome,
                (
                    target.map(|target| Target::parse(target)).or(default)?,
                    explicit,
                ),
            ))
        })
        .collect()
    }
}
