use std::collections::BTreeMap;

use serde::Serialize;

use super::spec::{Check, CheckKind, Step, StepKind, WorkflowSpec};

#[derive(Debug, Serialize)]
pub struct Edge {
    pub outcome: String,
    pub target: String,
    pub source: String,
}

impl WorkflowSpec {
    pub fn edges(&self, node: &str) -> Vec<Edge> {
        self.node_edges(node)
            .unwrap_or_default()
            .into_iter()
            .filter(|(outcome, _)| {
                self.steps
                    .get(node)
                    .is_none_or(|step| match outcome.as_str() {
                        "asked" => {
                            matches!(step.kind(), Some(StepKind::Agent(_)))
                                && step.lifecycle.unwrap_or_default().is_turn()
                        }
                        "capacity" => matches!(step.kind(), Some(StepKind::Agent(_))),
                        _ => true,
                    })
            })
            .map(|(outcome, (target, explicit))| Edge {
                outcome,
                target,
                source: if explicit { "explicit" } else { "default" }.into(),
            })
            .collect()
    }

    /// Resolve a routing decision against this spec, including engine-owned outcomes.
    pub fn target(&self, node: &str, outcome: &str) -> Option<String> {
        let edges = self.node_edges(node)?;
        if self.checks.contains_key(node) {
            return if outcome == "hold" {
                Some("@wait".into())
            } else {
                edges.get(outcome).map(|(target, _)| target.clone())
            };
        }
        let outcome = match outcome {
            "ended" | "died" | "canceled" | "limited" | "timeout" => "then",
            other => other,
        };
        edges
            .get(outcome)
            .map(|(target, _)| target.clone())
            .or_else(|| matches!(outcome, "then" | "asked").then(|| "@user".into()))
    }

    pub(super) fn engine_target(&self, node: &str, outcome: &str) -> String {
        // Engine callers have a current node and an engine-owned or normalized outcome.
        self.target(node, outcome).expect("admitted workflow route")
    }

    pub(super) fn node_edges(&self, node: &str) -> Option<BTreeMap<String, (String, bool)>> {
        let mut edges = if let Some(check) = self.checks.get(node) {
            check
                .edges()
                .into_iter()
                .map(|(outcome, target)| {
                    let explicit = check.on.contains_key(&outcome);
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
        Some(edges)
    }
}

impl Check {
    fn edges(&self) -> BTreeMap<String, String> {
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

impl Step {
    fn edges(&self) -> BTreeMap<&'static str, (&str, bool)> {
        [
            ("then", self.then.as_deref(), None),
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
