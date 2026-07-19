use super::*;

use crate::agents::AgentHookClass;
use serde_json::json;

mod ask;
mod context;
mod install;
mod lifecycle;

// Capability and coverage-table honesty is cross-checked against behavior for
// every adapter in `agents::conformance`; this slice only pins what is
// pi-specific behavior beyond those flags.

fn decode(event: &str, payload: &Value) -> HookOutput {
    PiAdapter
        .decode_hook(event, payload)
        .expect("pi hook decodes")
}

/// The lifecycle observation the decode produced. Panics on enrichment-only
/// events — use [`signal`] to assert that an event carries no observation.
fn observe(event: &str, payload: &Value) -> AgentLifecycleObservation {
    decode(event, payload)
        .lifecycle()
        .cloned()
        .expect("lifecycle observation")
}

fn signal(event: &str, payload: &Value) -> Option<LifecycleSignal> {
    decode(event, payload)
        .lifecycle()
        .map(|observation| observation.signal.clone())
}

#[test]
fn pi_activity_filter_excludes_the_blocking_gate() {
    // Completed-work events touch activity; the blocking `tool_call` gate is
    // excluded so creating the ask never instantly un-blocks the row.
    for event in [
        "tool_execution_end",
        "agent_end",
        "message_update",
        "turn_end",
    ] {
        assert!(
            decode(event, &json!({ "session_id": "sess-1" })).records_progress(),
            "{event} records progress"
        );
    }
    for event in ["tool_call", "session_shutdown"] {
        assert!(
            !decode(event, &json!({ "session_id": "sess-1" })).records_progress(),
            "{event} does not record progress"
        );
    }
}

#[test]
fn launch_resume_and_fork_argv_match_pi() {
    assert_eq!(
        PiAdapter.launch_command(&[], None),
        Some(vec!["pi".to_owned()])
    );
    assert_eq!(
        PiAdapter.launch_command(
            &["--model".to_owned(), "large".to_owned()],
            Some("review this"),
        ),
        Some(vec![
            "pi".to_owned(),
            "--model".to_owned(),
            "large".to_owned(),
            "--".to_owned(),
            "review this".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.resume_command("0199aaf2", Path::new("/tmp")),
        Some(vec![
            "pi".to_owned(),
            "--session".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.spec().launch.fork_command("0199aaf2"),
        Some(vec![
            "pi".to_owned(),
            "--fork".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
}

#[test]
fn neutral_decision_shape_is_pinned() {
    let rendered = decode("agent_end", &Value::Null).json_reply().cloned();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}
