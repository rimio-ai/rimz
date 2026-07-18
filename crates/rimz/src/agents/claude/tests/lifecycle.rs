use super::*;

#[test]
fn cursor_compatibility_hooks_are_quarantined() {
    let cursor = json!({ "cursor_version": "1.7.0", "conversation_id": "conv-1" });
    assert_eq!(
        ClaudeAdapter
            .decode_hook("PostToolUse", &cursor)
            .expect("test hook decodes")
            .class(),
        AgentHookClass::Unknown
    );
    assert_eq!(
        ClaudeAdapter
            .decode_hook("PostToolUse", &json!({ "session_id": "sess-1" }))
            .expect("test hook decodes")
            .class(),
        AgentHookClass::Lifecycle
    );
}

#[test]
fn final_message_fallback_reads_only_at_output_checkpoints() {
    use std::cell::Cell;

    let payload = json!({
        "session_id": "sess-1",
        "transcript_path": "/tmp/claude-session.jsonl"
    });
    let ordinary = AgentLifecycleObservation::new(
        None,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        },
    );
    let read = Cell::new(false);
    assert_eq!(
        final_message_for_lifecycle(&payload, &ordinary, |_| {
            read.set(true);
            None
        }),
        None
    );
    assert!(
        !read.get(),
        "ordinary tool hooks must not read the transcript"
    );

    let stopped = AgentLifecycleObservation::new(
        None,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    let message = final_message_for_lifecycle(&payload, &stopped, |_| {
        Some(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#
                .to_owned(),
        )
    });
    assert_eq!(message.as_deref(), Some("done"));

    assert_eq!(
        ClaudeAdapter
            .decode_hook(
                "PostToolUse",
                &json!({
                    "session_id": "sess-1",
                    "tool_name": "Bash",
                    "last_assistant_message": "must stay lazy"
                }),
            )
            .expect("test hook decodes")
            .final_message(),
        None
    );
    assert_eq!(
        ClaudeAdapter
            .decode_hook(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "last_assistant_message": "  final answer  "
                }),
            )
            .expect("test hook decodes")
            .final_message()
            .as_deref(),
        Some("final answer")
    );
}
use crate::agents::AgentHookClass;

#[test]
fn permission_request_does_not_duplicate_native_ask_tools() {
    for tool in ["AskUserQuestion", "ExitPlanMode"] {
        let payload = json!({ "session_id": "sess-1", "tool_name": tool });
        let classified = ClaudeAdapter
            .decode_hook("PermissionRequest", &payload)
            .expect("test hook decodes");
        assert_eq!(classified.class(), AgentHookClass::Lifecycle, "{tool}");
        assert_eq!(classified.ask_kind(), None, "{tool}");
        assert!(
            ClaudeAdapter
                .decode_hook("PermissionRequest", &payload)
                .expect("test hook decodes")
                .lifecycle()
                .is_none(),
            "{tool}"
        );
    }

    let payload = json!({ "session_id": "sess-1", "tool_name": "Bash" });
    let classified = ClaudeAdapter
        .decode_hook("PermissionRequest", &payload)
        .expect("test hook decodes");
    assert_eq!(classified.class(), AgentHookClass::AwaitingUser);
    assert_eq!(classified.ask_kind(), Some(AskKind::Permission));
    assert!(matches!(
        ClaudeAdapter
            .decode_hook("PermissionRequest", &payload)
            .expect("test hook decodes")
            .lifecycle()
            .map(|observation| observation.signal),
        Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ..
        })
    ));
}

#[test]
fn compaction_events_map_trigger_to_lifecycle_signals() {
    let pre = ClaudeAdapter
        .decode_hook("PreCompact", &json!({ "session_id": "sess-1" }))
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(pre.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(pre.signal, LifecycleSignal::Compacting);

    for (payload, expected) in [
        (
            json!({ "session_id": "sess-1", "trigger": "auto" }),
            LifecycleSignal::CompactionEnded { auto: Some(true) },
        ),
        (
            json!({ "session_id": "sess-1", "trigger": "manual" }),
            LifecycleSignal::CompactionEnded { auto: Some(false) },
        ),
        (
            json!({ "session_id": "sess-1", "trigger": "future" }),
            LifecycleSignal::CompactionEnded { auto: None },
        ),
        (
            json!({ "session_id": "sess-1" }),
            LifecycleSignal::CompactionEnded { auto: None },
        ),
    ] {
        let obs = ClaudeAdapter
            .decode_hook("PostCompact", &payload)
            .expect("test hook decodes")
            .lifecycle()
            .unwrap();
        assert_eq!(obs.signal, expected, "{payload}");
    }
}

#[test]
fn subagent_and_foreign_identity_boundaries_are_preserved() {
    let start = ClaudeAdapter
        .decode_hook(
            "SubagentStart",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "subagent_type": "Explore",
                "description": "search the store",
                "permission_mode": "acceptEdits",
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(start.agent_id.as_deref(), Some("child-1"));
    assert_eq!(start.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(start.task.as_deref(), Some("Explore"));
    assert_eq!(start.parent_agent_id.as_deref(), Some("sess-parent"));

    let stop_payload = json!({
        "session_id": "sess-parent",
        "agent_id": "child-1",
        "agent_type": "Explore",
        "last_assistant_message": "Analysis complete",
    });
    let stop = ClaudeAdapter
        .decode_hook("SubagentStop", &stop_payload)
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(stop.agent_id.as_deref(), Some("child-1"));
    assert_eq!(
        stop.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    assert_eq!(stop.task.as_deref(), Some("Explore"));
    assert_eq!(stop.parent_agent_id.as_deref(), Some("sess-parent"));

    let root = ClaudeAdapter
        .decode_hook("UserPromptSubmit", &json!({ "session_id": "sess-root" }))
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(root.agent_id.as_deref(), Some("sess-root"));
    assert_eq!(root.parent_agent_id, None);

    assert!(
        ClaudeAdapter
            .decode_hook(
                "SubagentStart",
                &json!({ "session_id": "sess-parent", "subagent_type": "Explore" }),
            )
            .expect("test hook decodes")
            .lifecycle()
            .is_none()
    );

    for (event, payload) in [
        (
            "PostToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "tool_name": "Edit",
            }),
        ),
        (
            "PreCompact",
            json!({ "session_id": "sess-parent", "agent_id": "child-1" }),
        ),
        (
            "PostCompact",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "trigger": "auto",
            }),
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "tool_name": "Read",
            }),
        ),
    ] {
        assert!(
            ClaudeAdapter
                .decode_hook(event, &payload)
                .expect("test hook decodes")
                .lifecycle()
                .is_none(),
            "{event}"
        );
    }

    let root_with_equal_id = ClaudeAdapter
        .decode_hook(
            "PostToolUse",
            &json!({
                "session_id": "sess-1",
                "agent_id": "sess-1",
                "tool_name": "Edit",
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(root_with_equal_id.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(root_with_equal_id.parent_agent_id, None);
}

#[test]
fn prompt_todo_and_tool_payloads_map_to_lifecycle_enrichment() {
    let control = ClaudeAdapter
        .decode_hook(
            "UserPromptSubmit",
            &json!({
                "session_id": "sess-1",
                "prompt": "<task-notification><task-id>afdc639e18e7ebdb9</task-id></task-notification>",
            }),
        ).expect("test hook decodes").lifecycle()
        .unwrap();
    assert_eq!(control.prompt, None);
    assert_eq!(control.task, None);

    let prompt = ClaudeAdapter
        .decode_hook(
            "UserPromptSubmit",
            &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(prompt.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.task.as_deref(), Some("fix auth flow"));

    for (tool, expected) in [
        (
            "Edit",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
                native_key: None,
            }),
        ),
        (
            "Bash",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                native_key: None,
            }),
        ),
        (
            "Read",
            Some(LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                native_key: None,
            }),
        ),
        (
            "AskUserQuestion",
            Some(LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                native_key: None,
            }),
        ),
    ] {
        let observed = ClaudeAdapter
            .decode_hook(
                "PostToolUse",
                &json!({ "session_id": "sess-1", "tool_name": tool }),
            )
            .expect("test hook decodes")
            .lifecycle();
        assert_eq!(observed.map(|obs| obs.signal), expected, "{tool}");
    }

    let pre_tool = ClaudeAdapter
        .decode_hook(
            "PreToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "Read" }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(
        pre_tool.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        }
    );
}

#[test]
fn session_start_stop_background_and_end_events_map_to_rollup_signals() {
    for (source, expected_signal, expected_origin) in [
        (
            "compact",
            LifecycleSignal::CompactionEnded { auto: None },
            None,
        ),
        (
            "startup",
            LifecycleSignal::Registered,
            Some(SessionOrigin::Fresh),
        ),
        ("resume", LifecycleSignal::Registered, None),
        (
            "clear",
            LifecycleSignal::Registered,
            Some(SessionOrigin::Fresh),
        ),
        ("future", LifecycleSignal::Registered, None),
    ] {
        let obs = ClaudeAdapter
            .decode_hook(
                "SessionStart",
                &json!({ "session_id": "sess-1", "source": source }),
            )
            .expect("test hook decodes")
            .lifecycle()
            .unwrap();
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        assert_eq!(obs.signal, expected_signal, "{source}");
        assert_eq!(obs.origin, expected_origin, "{source}");
        assert_eq!(obs.task, None);
    }
    let absent = ClaudeAdapter
        .decode_hook("SessionStart", &json!({ "session_id": "sess-1" }))
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(absent.signal, LifecycleSignal::Registered);
    assert_eq!(absent.origin, Some(SessionOrigin::Fresh));

    assert!(
        ClaudeAdapter
            .decode_hook("Notification", &json!({}))
            .expect("test hook decodes")
            .lifecycle()
            .is_none()
    );

    for (case, payload, expected_signal) in [
        (
            "clean stop",
            json!({ "session_id": "sess-1" }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        (
            "errored stop",
            json!({ "session_id": "sess-1", "is_error": true }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
        (
            "pending background task",
            json!({
                "session_id": "sess-1",
                "background_tasks": [
                    { "id": "task-1", "status": "running", "description": "Build process" }
                ]
            }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: true,
            },
        ),
        (
            "terminal background task",
            json!({
                "session_id": "sess-1",
                "background_tasks": [
                    { "id": "task-1", "status": "completed", "description": "Build process" }
                ]
            }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        (
            "scheduled wakeup",
            json!({
                "session_id": "sess-1",
                "session_crons": [
                    { "id": "cron-1", "schedule": "0 9 * * 1-5", "recurring": true, "prompt": "Check the build" }
                ]
            }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: true,
            },
        ),
        (
            "errored stop with pending background task",
            json!({
                "session_id": "sess-1",
                "is_error": true,
                "background_tasks": [
                    { "id": "task-1", "status": "running", "description": "Build process" }
                ]
            }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: true,
            },
        ),
    ] {
        let obs = ClaudeAdapter
            .decode_hook("Stop", &payload)
            .expect("test hook decodes")
            .lifecycle()
            .unwrap_or_else(|| panic!("{case} should produce a lifecycle observation"));
        assert_eq!(obs.signal, expected_signal, "{case}");
        assert_eq!(obs.task, None, "{case}");
    }

    let decoded = ClaudeAdapter
        .decode_hook("SessionEnd", &json!({ "session_id": "sess-1" }))
        .expect("test hook decodes");
    let ended = decoded
        .lifecycle()
        .expect("SessionEnd is a recorded lifecycle observation");
    assert_eq!(ended.agent_id.as_deref(), Some("sess-1"));
    assert!(decoded.ends_session());
    assert!(
        !ClaudeAdapter
            .decode_hook("Stop", &json!({ "session_id": "sess-1" }))
            .expect("test hook decodes")
            .ends_session()
    );
}

#[test]
fn expiry_predicates_match_observed_root_signals() {
    for (event, payload) in [
        ("SessionStart", json!({ "session_id": "sess-1" })),
        (
            "SessionStart",
            json!({ "session_id": "sess-1", "source": "compact" }),
        ),
        ("UserPromptSubmit", json!({ "session_id": "sess-1" })),
        ("Stop", json!({ "session_id": "sess-1" })),
        ("SessionEnd", json!({ "session_id": "sess-1" })),
        (
            "PostToolUse",
            json!({ "session_id": "sess-1", "tool_name": "Edit" }),
        ),
        ("PreToolUse", json!({ "session_id": "sess-1" })),
        ("PreCompact", json!({ "session_id": "sess-1" })),
        ("PostCompact", json!({ "session_id": "sess-1" })),
    ] {
        let decoded = ClaudeAdapter
            .decode_hook(event, &payload)
            .expect("test hook decodes");
        let obs = decoded
            .lifecycle()
            .unwrap_or_else(|| panic!("{event} should be observed"));
        assert_eq!(
            decoded.ends_session(),
            matches!(obs.signal, LifecycleSignal::Ended),
            "{event} session-end predicate"
        );
    }
}
