use super::*;

use crate::agents::AgentStatus;
use crate::agents::lifecycle::{LifecycleState, TurnPhase, step};

#[test]
fn lifecycle_signals_map_every_wired_event() {
    let session = json!({ "session_id": "sess-1" });
    for (event, payload, expected) in [
        (
            "session_start",
            session.clone(),
            Some(LifecycleSignal::Registered),
        ),
        (
            "before_agent_start",
            json!({ "session_id": "sess-1", "prompt": "fix auth" }),
            Some(LifecycleSignal::TurnStarted),
        ),
        (
            "session_before_compact",
            session.clone(),
            Some(LifecycleSignal::Compacting),
        ),
        (
            "session_shutdown",
            session.clone(),
            Some(LifecycleSignal::Ended),
        ),
        (
            "tool_execution_end",
            json!({ "session_id": "sess-1", "tool_call_id": "sibling-call", "tool_name": "edit" }),
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
                name: Some("edit".to_owned()),
                native_key: Some("sibling-call".to_owned()),
            }),
        ),
        (
            "tool_execution_end",
            json!({ "session_id": "sess-1", "tool_call_id": "sibling-call", "tool_name": "bash" }),
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                name: Some("bash".to_owned()),
                native_key: Some("sibling-call".to_owned()),
            }),
        ),
        (
            "tool_execution_end",
            json!({ "session_id": "sess-1", "tool_name": "read" }),
            Some(LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: Some("read".to_owned()),
                native_key: None,
            }),
        ),
        // An ordinary tool call is neutral; only the rpiv questionnaire blocks.
        ("tool_call", session.clone(), None),
        ("bogus", json!({}), None),
    ] {
        assert_eq!(signal(event, &payload), expected, "{event} {payload}");
    }

    // A mutating tool moves the turn out of reasoning and into acting.
    let running = LifecycleState {
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        compacting: false,
    };
    let edit = observe(
        "tool_execution_end",
        &json!({ "session_id": "sess-1", "tool_name": "edit" }),
    );
    assert_eq!(
        step(Some(&running), None, &edit.signal).next.phase,
        TurnPhase::Acting
    );
}

/// Regression, `9acad2888 fix(pi): follow final settled lifecycle`. Pi reports
/// its verdict on `agent_end` but keeps working; only `agent_settled` means the
/// turn is over. Ending on `agent_end` cut turns short and completed supervised
/// output early.
#[test]
fn settled_boundary_is_terminal_and_agent_end_is_not() {
    assert_eq!(
        signal(
            "agent_end",
            &json!({ "session_id": "sess-1", "stop_reason": "error" }),
        ),
        None
    );
    assert!(!decode("agent_end", &json!({ "session_id": "sess-1" })).ends_session());

    for (payload, expected) in [
        (
            json!({ "session_id": "sess-1", "stop_reason": "stop" }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "stop_reason": "aborted" }),
            LifecycleSignal::TurnInterrupted,
        ),
        (
            json!({ "session_id": "sess-1", "stop_reason": "error" }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "stop_reason": "stop", "error_message": "boom" }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
    ] {
        assert_eq!(
            observe("agent_settled", &payload).signal,
            expected,
            "payload {payload}",
        );
    }
}

/// Regression, `9acad2888 fix(pi): follow final settled lifecycle`. Pi added
/// the compaction cause in 0.79.10; older releases omit it, and an unknown
/// cause must stay unknown rather than guessing automatic.
#[test]
fn compaction_end_reports_its_cause() {
    for (reason, expected) in [
        (Some("manual"), Some(false)),
        (Some("threshold"), Some(true)),
        (Some("overflow"), Some(true)),
        (Some("future"), None),
        (None, None),
    ] {
        let mut payload = json!({ "session_id": "sess-1" });
        if let Some(reason) = reason {
            payload["compaction_reason"] = json!(reason);
        }
        assert_eq!(
            observe("session_compact", &payload).signal,
            LifecycleSignal::CompactionEnded {
                auto: expected,
                failed: false,
            },
            "{reason:?}"
        );
    }
}

#[test]
fn compaction_failure_reports_its_cause() {
    for (reason, expected) in [
        (Some("manual"), Some(false)),
        (Some("threshold"), Some(true)),
        (Some("overflow"), Some(true)),
        (Some("future"), None),
        (None, None),
    ] {
        let mut payload = json!({ "session_id": "sess-1" });
        if let Some(reason) = reason {
            payload["compaction_reason"] = json!(reason);
        }
        assert_eq!(
            observe("session_compact_failed", &payload).signal,
            LifecycleSignal::CompactionEnded {
                auto: expected,
                failed: true,
            },
            "{reason:?}"
        );
    }
}

#[test]
fn observation_carries_prompt_model_effort_and_usage() {
    let started = observe(
        "session_start",
        &json!({
            "session_id": "sess-1",
            "cwd": "/home/u/code/query-engine",
            "model": "gpt-5.5",
            "effort": "medium",
            "context_pct": 150,
            "context_window": 272_000,
            "total_tokens": 8160,
        }),
    );
    assert_eq!(started.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(
        started.worktree_path.as_deref(),
        Some("/home/u/code/query-engine")
    );
    assert_eq!(started.launch.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(started.launch.effort.as_deref(), Some("medium"));
    // An out-of-range gauge clamps rather than rendering past full.
    assert_eq!(started.usage.context_pct, Some(100));
    assert_eq!(started.usage.context_window, Some(272_000));
    assert_eq!(started.usage.total_tokens, Some(8160));
    assert_eq!(started.parent_agent_id, None);

    let prompt = observe(
        "before_agent_start",
        &json!({ "session_id": "sess-1", "prompt": "  add a dark mode toggle  " }),
    );
    assert_eq!(prompt.prompt.as_deref(), Some("add a dark mode toggle"));
    assert_eq!(prompt.task.as_deref(), Some("add a dark mode toggle"));

    // Regression, `e78b7fd40 fix(agents): reject skill prompts as
    // descriptions`: a skill envelope is harness plumbing, not a user prompt,
    // and must never surface as the card description.
    let skill = observe(
        "before_agent_start",
        &json!({
            "session_id": "sess-1",
            "prompt": "<skill name=\"merge\" Location=\"/home/u/.agents/skills/merge/SKILL.md\">\nmerge the branch\n</skill>"
        }),
    );
    assert_eq!(skill.prompt, None);
    assert_eq!(skill.task, None);

    let settled = observe(
        "agent_settled",
        &json!({
            "session_id": "sess-1",
            "stop_reason": "stop",
            "model": "gpt-5",
            "total_tokens": 4200,
            "input_tokens": 100,
            "cache_write_input_tokens": 40,
            "cache_read_input_tokens": 30,
            "output_tokens": 20,
        }),
    );
    assert_eq!(settled.launch.model.as_deref(), Some("gpt-5"));
    assert_eq!(settled.usage.total_tokens, Some(4200));
    assert_eq!(settled.usage.fresh_input_tokens, Some(100));
    assert_eq!(settled.usage.cache_write_input_tokens, Some(40));
    assert_eq!(settled.usage.cache_read_input_tokens, Some(30));
    assert_eq!(settled.usage.output_tokens, Some(20));
}

/// Regression, `9acad2888 fix(pi): follow final settled lifecycle`. Supervised
/// runs complete on the final message, so taking it from `agent_end` truncated
/// the run before pi had finished.
#[test]
fn final_message_lands_only_on_the_settled_boundary() {
    let payload = json!({
        "session_id": "sess-1",
        "last_assistant_message": "  Fixed the parser.  "
    });
    assert_eq!(
        decode("agent_settled", &payload).final_message(),
        Some("Fixed the parser.")
    );
    assert_eq!(
        decode("agent_end", &payload).final_message(),
        None,
        "agent_end is enrichment-only and must not complete output early"
    );
}

#[test]
fn subagent_lifecycle_normalizes_identity_and_quarantines_drift() {
    let started = observe(
        "subagent_started",
        &json!({
            "session_id": "parent-1",
            "cwd": "/work/project",
            "subagent_id": "run-7#1",
            "subagent_label": " reviewer ",
            "subagent_source": "pi-session"
        }),
    );
    assert_eq!(started.agent_id.as_deref(), Some("run-7#1"));
    assert_eq!(started.parent_agent_id.as_deref(), Some("parent-1"));
    assert_eq!(started.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(started.task.as_deref(), Some("reviewer"));
    assert_eq!(started.worktree_path.as_deref(), Some("/work/project"));
    assert_eq!(started.usage.total_tokens, None);

    let stopped = observe(
        "subagent_stopped",
        &json!({
            "session_id": "parent-1",
            "cwd": "/work/project",
            "subagent_id": "run-7#1",
            "subagent_label": "reviewer",
            "subagent_source": "pi-session",
            "errored": true,
            "total_tokens": 1234
        }),
    );
    assert_eq!(stopped.agent_id.as_deref(), Some("run-7#1"));
    assert_eq!(stopped.parent_agent_id.as_deref(), Some("parent-1"));
    assert_eq!(
        stopped.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert_eq!(stopped.task.as_deref(), Some("reviewer"));
    assert_eq!(stopped.usage.total_tokens, Some(1234));

    // Regression, `985018e6d fix(pi): unify tintinweb child session rows`: a
    // child that cannot name a distinct parent is dropped rather than
    // materialized as an orphan or a self-parented row.
    for payload in [
        json!({
            "session_id": "parent-1",
            "subagent_label": "missing child"
        }),
        json!({
            "session_id": "same-id",
            "subagent_id": "same-id",
            "subagent_label": "same child and parent"
        }),
    ] {
        assert_eq!(
            signal("subagent_started", &payload),
            None,
            "payload {payload}"
        );
    }
}
