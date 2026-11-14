use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::definition::{Definition, Kind, Target};
use super::spec::Cause;

/// The outcome naming each edge from a session to one of its rest checks.
const REST_CHECK: &str = "rest-check";

/// Where an edge's target came from: the node's own route, the fallback chain's next level,
/// or the run's end when nothing routes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum EdgeSource {
    #[serde(rename = "explicit")]
    Explicit,
    #[serde(rename = "on.failed")]
    NodeFailed,
    #[serde(rename = "workflow.on.failed")]
    WorkflowFailed,
    #[serde(rename = "default")]
    Default,
}

impl std::fmt::Display for EdgeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Explicit => "explicit",
            Self::NodeFailed => "on.failed",
            Self::WorkflowFailed => "workflow.on.failed",
            Self::Default => "default",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fireability {
    Yes,
    Never,
    WithDeadline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NeverReason {
    NoTimeout,
    NoMaxVisits,
}

impl NeverReason {
    fn mark(self) -> &'static str {
        match self {
            Self::NoTimeout => "never: no timeout",
            Self::NoMaxVisits => "never: no max-visits",
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
        let order = self.walk_component(self.start(), &mut seen);
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
                if edge.fires != Fireability::Never
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
                let step = self.step(node)?;
                let parent = step
                    .rest_check()
                    .iter()
                    .chain(step.then())
                    .find_map(|parent| {
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
        self.resolved_edges(node).unwrap_or_default().to_vec()
    }

    /// Resolve a routing decision against this definition, including engine-owned outcomes:
    /// a cause, or a step attempt's ending, takes the failure chain.
    pub fn target(&self, node: &str, outcome: &str) -> Option<Target> {
        let edges = self.resolved_edges(node)?;
        if let Some(cause) = self.ending_cause(node, outcome) {
            return Some(self.failure_target(node, cause));
        }
        let step = self.check(node).is_none();
        let outcome = match outcome {
            "hold" if !step => return Some(Target::Wait),
            "ended" if step => "then",
            other => other,
        };
        edges
            .iter()
            .find(|edge| edge.outcome == outcome)
            .map(|edge| edge.target.clone())
            .or_else(|| (step && outcome == "then").then_some(Target::Failed))
    }

    /// The cause an outcome at `node` raises: a cause word as written, or a step attempt's
    /// ending that is not clean (a message's undelivered, any other step's died).
    pub(super) fn ending_cause(&self, node: &str, outcome: &str) -> Option<Cause> {
        if self.check(node).is_none() && matches!(outcome, "died" | "canceled") {
            return Some(match self.step_kind(node) {
                Some(Kind::Message(_)) => Cause::Undelivered,
                _ => Cause::Died,
            });
        }
        Cause::parse(outcome)
    }

    /// The fallback chain: the node's `on.<cause>`, else its `on.failed`, else the workflow's
    /// `on.failed`, else the run fails.
    pub(super) fn failure_route(&self, node: &str, cause: Cause) -> (Target, EdgeSource) {
        let authored = self.authored();
        let (own, failed) = match (authored.checks.get(node), authored.steps.get(node)) {
            (Some(check), _) => (check.on.get(cause.word()), check.on.get("failed")),
            (None, Some(step)) => (step.on.causes.get(cause.word()), step.on.failed.as_ref()),
            (None, None) => (None, None),
        };
        if let Some(route) = own {
            (Target::parse(route), EdgeSource::Explicit)
        } else if let Some(route) = failed {
            (Target::parse(route), EdgeSource::NodeFailed)
        } else if let Some(route) = &authored.workflow.on.failed {
            (Target::parse(route), EdgeSource::WorkflowFailed)
        } else {
            (Target::Failed, EdgeSource::Default)
        }
    }

    pub(super) fn failure_target(&self, node: &str, cause: Cause) -> Target {
        self.failure_route(node, cause).0
    }

    pub(super) fn engine_target(&self, node: &str, outcome: &str) -> Target {
        // Engine callers have a current node and an engine-owned or normalized outcome.
        self.target(node, outcome).expect("admitted workflow route")
    }

    /// Every edge out of `node`, resolved at compile; `None` when no node has the name.
    pub(super) fn resolved_edges(&self, node: &str) -> Option<&[Edge]> {
        self.node(node).map(super::definition::Node::resolved_edges)
    }

    /// The node's answer routes in display order, then one edge per cause its kind raises.
    pub(super) fn compile_edges(&self, node: &str) -> Option<Vec<Edge>> {
        let authored = self.authored();
        let mut edges = Vec::new();
        let mut answer = |outcome: &str, target: Target, source: EdgeSource| {
            edges.push(Edge {
                outcome: outcome.into(),
                target,
                source,
                fires: Fireability::Yes,
                because: None,
            });
        };
        let table = if let Some(check) = authored.checks.get(node) {
            if check.is_sleep() {
                if let Some(then) = &check.then {
                    answer("then", Target::parse(then), EdgeSource::Explicit);
                }
                answer("sleeping", Target::Wait, EdgeSource::Default);
                if check.when.is_some() {
                    answer("unknown", Target::Wait, EdgeSource::Default);
                }
            } else {
                let mut seen = BTreeSet::new();
                let outcomes = check
                    .cases
                    .iter()
                    .map(|case| case.outcome.as_str())
                    .chain(check.else_outcome.as_deref())
                    .chain(check.on.keys().map(String::as_str))
                    .filter(|outcome| *outcome != "failed" && Cause::parse(outcome).is_none());
                for outcome in outcomes {
                    if let Some(route) = check.on.get(outcome)
                        && seen.insert(outcome)
                    {
                        answer(outcome, Target::parse(route), EdgeSource::Explicit);
                    }
                }
            }
            check.table.word()
        } else {
            let step = authored.steps.get(node)?;
            if let Some(then) = step.then.first() {
                answer("then", Target::parse(then), EdgeSource::Explicit);
            }
            if let Some(asked) = &step.on.asked
                && step.table == super::spec::StepTable::Agent
            {
                answer("asked", Target::parse(asked), EdgeSource::Explicit);
            }
            step.table.word()
        };
        for cause in Cause::raised_by(table) {
            let (target, source) = self.failure_route(node, *cause);
            let (fires, because) = self.fireability(node, *cause);
            edges.push(Edge {
                outcome: cause.word().into(),
                target,
                source,
                fires,
                because,
            });
        }
        // A session's rest checks run at its rests, so the walk and the cycle bound see each.
        let rest_checks = authored
            .steps
            .get(node)
            .and_then(|step| step.rest_check.as_ref());
        edges.extend(rest_checks.into_iter().flatten().map(|check| Edge {
            outcome: REST_CHECK.into(),
            target: Target::parse(check),
            source: EdgeSource::Explicit,
            fires: Fireability::Yes,
            because: None,
        }));
        Some(edges)
    }

    fn fireability(&self, node: &str, cause: Cause) -> (Fireability, Option<NeverReason>) {
        let authored = self.authored();
        let (timeout, max_visits) = match (authored.checks.get(node), authored.steps.get(node)) {
            (Some(check), _) => (check.timeout.is_some(), false),
            (None, Some(step)) => (step.timeout.is_some(), step.max_visits.is_some()),
            (None, None) => (true, true),
        };
        match cause {
            Cause::Deadline if authored.workflow.deadline.is_none() => {
                (Fireability::WithDeadline, None)
            }
            Cause::Timeout if !timeout => (Fireability::Never, Some(NeverReason::NoTimeout)),
            Cause::Visits if !max_visits => (Fireability::Never, Some(NeverReason::NoMaxVisits)),
            _ => (Fireability::Yes, None),
        }
    }
}
