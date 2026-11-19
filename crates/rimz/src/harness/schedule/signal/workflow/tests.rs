use super::*;
use crate::store::event::SignalSource;
use crate::store::workflow::RunState;
use serde_json::{Value, json};

fn record(node: &str, state: &str) -> RunRecord {
    serde_json::from_value(json!({
        "version": crate::store::workflow::VERSION,
        "id": "wf_0190b8172c0070008000000000000001",
        "workflow": "review", "spec_dir": "/defs/review", "prompt": "Run request",
        "project_root": "/repo", "scope": {"kind": "worktree", "path": "/repo/wt"},
        "created_at": "2026-01-01T00:00:00Z", "state": state, "node": node,
        "entered_at": "2026-01-01T00:00:00Z", "deadline_fired": false, "next_seq": 1,
        "visits": {}, "visit_grant": 0
    }))
    .unwrap()
}

fn names(signals: &[Signal]) -> Vec<&str> {
    signals.iter().map(|signal| signal.name.as_str()).collect()
}

#[test]
fn each_committed_edge_fires_its_signals_once() {
    let active = |node| record(node, "active");
    let cases: [(RunRecord, RunRecord, &[&str]); 9] = [
        (active("a"), active("b"), &["workflow.moved"]),
        (active("a"), record("a", "done"), &["workflow.ended"]),
        (active("a"), record("@done", "done"), &["workflow.ended"]),
        (
            active("a"),
            record("a", "failed"),
            &["workflow.ended", "workflow.failed"],
        ),
        (active("a"), record("a", "canceled"), &["workflow.ended"]),
        (active("a"), record("a", "parked"), &["workflow.waiting"]),
        (
            active("a"),
            record("b", "parked"),
            &["workflow.moved", "workflow.waiting"],
        ),
        (record("a", "parked"), active("a"), &[]),
        (record("a", "failed"), record("a", "failed"), &[]),
    ];
    for (before, after, expected) in cases {
        let fired = run_signals(&before, &after);
        assert_eq!(
            names(&fired),
            expected,
            "{:?} {} -> {:?} {}",
            before.state,
            before.node,
            after.state,
            after.node
        );
        assert!(
            fired
                .iter()
                .all(|signal| signal.source == SignalSource::Lifecycle),
            "{fired:?}"
        );
    }
    assert!(run_signals(&active("a"), &active("a")).is_empty());
}

#[test]
fn a_loop_tasks_own_command_run_fires_nothing() {
    let inline = |node, state| {
        let mut record = record(node, state);
        record.origin = crate::store::workflow::RunOrigin::Inline;
        record
    };
    assert!(run_signals(&inline("command", "active"), &inline("command", "done")).is_empty());
    assert!(run_signals(&inline("command", "active"), &inline("command", "failed")).is_empty());
}

#[test]
fn a_signal_names_the_run_its_node_and_its_checkout() {
    let payload = |signal: &Signal| Value::Object(signal.payload.clone());
    let moved = run_signals(&record("a", "active"), &record("b", "active"));
    assert_eq!(
        payload(&moved[0]),
        json!({
            "run": "wf_0190b8172c0070008000000000000001", "workflow": "review", "node": "b",
            "from": "a", "checkout": "/repo/wt"
        })
    );
    let ended = run_signals(&record("b", "active"), &record("b", "failed"));
    for signal in &ended {
        assert_eq!(
            payload(signal),
            json!({
                "run": "wf_0190b8172c0070008000000000000001", "workflow": "review", "node": "b",
                "outcome": "failed", "checkout": "/repo/wt"
            })
        );
    }
}

#[test]
fn a_stranded_run_ends_failed_unless_it_had_ended() {
    let id: WorkflowRunId = "wf_0190b8172c0070008000000000000001".parse().unwrap();
    let header = RunHeader {
        workflow: Some("review".into()),
        node: Some("b".into()),
        state: Some(RunState::Active),
        scope_path: Some("/repo/wt".into()),
        ..RunHeader::default()
    };
    let fired = stranded_signals(&id, &header);
    assert_eq!(names(&fired), ["workflow.ended", "workflow.failed"]);
    assert_eq!(
        Value::Object(fired[0].payload.clone()),
        json!({
            "run": "wf_0190b8172c0070008000000000000001", "workflow": "review", "node": "b",
            "outcome": "failed", "checkout": "/repo/wt"
        })
    );
    // A record that names no origin predates inline runs, so it is a deployed run's.
    let deployed = RunHeader {
        origin: RunOrigin::Deployed,
        ..header.clone()
    };
    assert_eq!(names(&stranded_signals(&id, &deployed)), names(&fired));
    // A loop shell task's own command run fires nothing when it strands, as while it ran.
    let inline = RunHeader {
        origin: RunOrigin::Inline,
        ..header.clone()
    };
    assert!(stranded_signals(&id, &inline).is_empty());
    let ended = RunHeader {
        state: Some(RunState::Done),
        ..header
    };
    assert!(stranded_signals(&id, &ended).is_empty());
}
