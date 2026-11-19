//! Run lifecycle edges read from a workflow run's committed record before and after one write.

use serde_json::{Map, Value, json};

use crate::harness::signal::Signal;
use crate::ids::WorkflowRunId;
use crate::store::event::SignalSource;
use crate::store::workflow::{RunHeader, RunOrigin, RunRecord, RunState};

/// The `workflow.*` signals one committed write of a run fires: `workflow.ended` when it
/// becomes terminal, with `workflow.failed` beside it when it failed; `workflow.moved` when its
/// node changes and it goes on; `workflow.waiting` when it starts to need a person. A loop
/// task's own command run fires none, or a task on `workflow.ended` would fire itself again at
/// the end of each of its own runs.
pub fn run_signals(before: &RunRecord, after: &RunRecord) -> Vec<Signal> {
    if after.origin == RunOrigin::Inline {
        return Vec::new();
    }
    let mut names = Vec::new();
    let ended = !before.state.is_terminal() && after.state.is_terminal();
    if !after.state.is_terminal() && before.node != after.node {
        names.push("workflow.moved");
    }
    if ended {
        names.push("workflow.ended");
        if after.state == RunState::Failed {
            names.push("workflow.failed");
        }
    }
    if !before.needs_you() && after.needs_you() {
        names.push("workflow.waiting");
    }
    let edge = Edge {
        run: &after.id,
        workflow: Some(&after.workflow),
        node: Some(&after.node),
        checkout: Some(&after.scope.path),
    };
    names
        .into_iter()
        .map(|name| {
            let mut payload = edge.payload();
            if name == "workflow.moved" {
                payload.insert("from".into(), json!(before.node));
            }
            if matches!(name, "workflow.ended" | "workflow.failed") {
                payload.insert("outcome".into(), json!(after.state));
            }
            signal(name, payload)
        })
        .collect()
}

/// The `workflow.*` signals the room's workflow pass fires when it ends a stranded run as
/// failed, read from the header the pass read before the ending; none for a run that had ended.
pub fn stranded_signals(id: &WorkflowRunId, header: &RunHeader) -> Vec<Signal> {
    if header.state.is_some_and(RunState::is_terminal) || header.origin == RunOrigin::Inline {
        return Vec::new();
    }
    let mut payload = Edge {
        run: id,
        workflow: header.workflow.as_deref(),
        node: header.node.as_deref(),
        checkout: header.scope_path.as_deref(),
    }
    .payload();
    payload.insert("outcome".into(), json!(RunState::Failed));
    ["workflow.ended", "workflow.failed"]
        .into_iter()
        .map(|name| signal(name, payload.clone()))
        .collect()
}

/// What every `workflow.*` payload names.
struct Edge<'a> {
    run: &'a WorkflowRunId,
    workflow: Option<&'a str>,
    node: Option<&'a str>,
    checkout: Option<&'a std::path::Path>,
}

impl Edge<'_> {
    fn payload(&self) -> Map<String, Value> {
        let mut payload = Map::from_iter([("run".to_owned(), json!(self.run))]);
        for (key, value) in [
            ("workflow", self.workflow.map(Value::from)),
            ("node", self.node.map(Value::from)),
            ("checkout", self.checkout.map(|path| json!(path))),
        ] {
            if let Some(value) = value {
                payload.insert(key.to_owned(), value);
            }
        }
        payload
    }
}

fn signal(name: &str, payload: Map<String, Value>) -> Signal {
    Signal {
        // Only the four valid static signal names above reach this parser.
        name: name.parse().expect("static workflow signal name is valid"),
        payload,
        source: SignalSource::Lifecycle,
        watch: None,
    }
}

#[cfg(test)]
mod tests;
