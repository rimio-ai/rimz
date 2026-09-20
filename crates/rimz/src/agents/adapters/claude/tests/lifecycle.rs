use super::*;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output};
use crate::ids::AgentSessionId;

#[test]
fn cursor_compatibility_hooks_are_quarantined() {
    let cursor = json!({ "cursor_version": "1.7.0", "conversation_id": "conv-1" });
    assert_eq!(
        hook_output(&ClaudeAdapter, "PostToolUse", &cursor).class(),
        AgentHookClass::Unknown
    );
    assert_eq!(
        hook_output(
            &ClaudeAdapter,
            "PostToolUse",
            &json!({ "session_id": "sess-1" })
        )
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
            name: None,
            native_key: None,
            turn_id: None,
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
            turn_id: None,
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
        hook_output(
            &ClaudeAdapter,
            "PostToolUse",
            &json!({
                "session_id": "sess-1",
                "tool_name": "Bash",
                "last_assistant_message": "must stay lazy"
            })
        )
        .final_message()
        .map(str::to_owned),
        None
    );
    assert_eq!(
        hook_output(
            &ClaudeAdapter,
            "Stop",
            &json!({
                "session_id": "sess-1",
                "last_assistant_message": "  final answer  "
            })
        )
        .final_message()
        .map(str::to_owned)
        .as_deref(),
        Some("final answer")
    );
}
use crate::agents::AgentHookClass;

#[test]
fn permission_request_does_not_duplicate_native_ask_tools() {
    for tool in ["AskUserQuestion", "ExitPlanMode"] {
        let payload = json!({ "session_id": "sess-1", "tool_name": tool });
        let classified = hook_output(&ClaudeAdapter, "PermissionRequest", &payload);
        assert_eq!(classified.class(), AgentHookClass::Lifecycle, "{tool}");
        assert_eq!(classified.ask_kind(), None, "{tool}");
        assert!(
            hook_observation(&ClaudeAdapter, "PermissionRequest", &payload).is_none(),
            "{tool}"
        );
    }

    let payload = json!({ "session_id": "sess-1", "tool_name": "Bash" });
    let classified = hook_output(&ClaudeAdapter, "PermissionRequest", &payload);
    assert_eq!(classified.class(), AgentHookClass::AwaitingUser);
    assert_eq!(classified.ask_kind(), Some(AskKind::Permission));
    assert_eq!(classified.ask_detail(), Some("Bash"));
    assert!(matches!(
        hook_observation(&ClaudeAdapter, "PermissionRequest", &payload)
            .map(|observation| observation.signal),
        Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ..
        })
    ));

    for payload in [
        json!({"session_id": "sess-1", "tool_name": " Bash ", "tool_input": {}}),
        json!({"session_id": "sess-1", "tool_name": " Bash ", "tool_input": null}),
    ] {
        assert_eq!(
            hook_output(&ClaudeAdapter, "PermissionRequest", &payload).ask_detail(),
            Some("Bash")
        );
    }

    let detail = hook_output(
        &ClaudeAdapter,
        "PermissionRequest",
        &json!({
            "session_id": "sess-1",
            "tool_name": "Bash",
            "tool_input": {"command": "x".repeat(200)}
        }),
    )
    .ask_detail()
    .expect("detail")
    .to_owned();
    assert_eq!(
        detail
            .strip_prefix("Bash: ")
            .expect("summary")
            .chars()
            .count(),
        160
    );
}

#[test]
fn compaction_events_map_trigger_to_lifecycle_signals() {
    let pre = hook_lifecycle(
        &ClaudeAdapter,
        "PreCompact",
        &json!({ "session_id": "sess-1" }),
    );
    assert_eq!(pre.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(pre.signal, LifecycleSignal::Compacting);

    for (payload, expected) in [
        (
            json!({ "session_id": "sess-1", "trigger": "auto" }),
            LifecycleSignal::CompactionEnded {
                auto: Some(true),
                failed: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "trigger": "manual" }),
            LifecycleSignal::CompactionEnded {
                auto: Some(false),
                failed: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "trigger": "future" }),
            LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
            },
        ),
        (
            json!({ "session_id": "sess-1" }),
            LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
            },
        ),
    ] {
        let obs = hook_lifecycle(&ClaudeAdapter, "PostCompact", &payload);
        assert_eq!(obs.signal, expected, "{payload}");
    }
}

#[test]
fn subagent_and_foreign_identity_boundaries_are_preserved() {
    let start = hook_lifecycle(
        &ClaudeAdapter,
        "SubagentStart",
        &json!({
            "session_id": "sess-parent",
            "agent_id": "child-1",
            "subagent_type": "Explore",
            "description": "search the store",
            "permission_mode": "acceptEdits",
        }),
    );
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
    let stop = hook_lifecycle(&ClaudeAdapter, "SubagentStop", &stop_payload);
    assert_eq!(stop.agent_id.as_deref(), Some("child-1"));
    assert_eq!(
        stop.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    assert_eq!(stop.task.as_deref(), Some("Explore"));
    assert_eq!(stop.parent_agent_id.as_deref(), Some("sess-parent"));

    let root = hook_lifecycle(
        &ClaudeAdapter,
        "UserPromptSubmit",
        &json!({ "session_id": "sess-root" }),
    );
    assert_eq!(root.agent_id.as_deref(), Some("sess-root"));
    assert_eq!(root.parent_agent_id, None);

    let agent_mode_root = hook_lifecycle(
        &ClaudeAdapter,
        "UserPromptSubmit",
        &json!({
            "session_id": "sess-agent-mode",
            "agent_type": "code-reviewer",
        }),
    );
    assert_eq!(agent_mode_root.agent_id.as_deref(), Some("sess-agent-mode"));
    assert_eq!(agent_mode_root.parent_agent_id, None);

    assert!(
        hook_observation(
            &ClaudeAdapter,
            "SubagentStart",
            &json!({ "session_id": "sess-parent", "subagent_type": "Explore" })
        )
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
        let observation = hook_lifecycle(&ClaudeAdapter, event, &payload);
        assert_eq!(observation.agent_id.as_deref(), Some("child-1"), "{event}");
        assert_eq!(
            observation.parent_agent_id.as_deref(),
            Some("sess-parent"),
            "{event}"
        );
    }

    let root_with_equal_id = hook_lifecycle(
        &ClaudeAdapter,
        "PostToolUse",
        &json!({
            "session_id": "sess-1",
            "agent_id": "sess-1",
            "tool_name": "Edit",
        }),
    );
    assert_eq!(root_with_equal_id.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(root_with_equal_id.parent_agent_id, None);
}

#[test]
fn subagent_blocking_asks_use_child_identity() {
    for (event, tool_name, expected_kind) in [
        ("PermissionRequest", "Bash", AskKind::Permission),
        ("PreToolUse", "AskUserQuestion", AskKind::Question),
    ] {
        let observation = hook_lifecycle(
            &ClaudeAdapter,
            event,
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "tool_name": tool_name,
                "tool_input": {},
            }),
        );
        assert_eq!(observation.agent_id.as_deref(), Some("child-1"));
        assert_eq!(observation.parent_agent_id.as_deref(), Some("sess-parent"));
        assert!(matches!(
            observation.signal,
            LifecycleSignal::AwaitingInput { kind, .. } if kind == expected_kind
        ));
    }
}

#[test]
fn prompt_todo_and_tool_payloads_map_to_lifecycle_enrichment() {
    let control = hook_lifecycle(
        &ClaudeAdapter,
        "UserPromptSubmit",
        &json!({
            "session_id": "sess-1",
            "prompt": "<task-notification><task-id>afdc639e18e7ebdb9</task-id></task-notification>",
        }),
    );
    assert_eq!(control.prompt, None);
    assert_eq!(control.task, None);

    let prompt = hook_lifecycle(
        &ClaudeAdapter,
        "UserPromptSubmit",
        &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
    );
    assert_eq!(prompt.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(
        prompt.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
    assert_eq!(prompt.task.as_deref(), Some("fix auth flow"));

    for (tool, expected) in [
        (
            "Edit",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
                name: Some("Edit".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "Bash",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                name: Some("Bash".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "Read",
            Some(LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: Some("Read".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "AskUserQuestion",
            Some(LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: Some("AskUserQuestion".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
    ] {
        let observed = hook_observation(
            &ClaudeAdapter,
            "PostToolUse",
            &json!({ "session_id": "sess-1", "tool_name": tool }),
        );
        assert_eq!(observed.map(|obs| obs.signal), expected, "{tool}");
    }

    let pre_tool = hook_lifecycle(
        &ClaudeAdapter,
        "PreToolUse",
        &json!({ "session_id": "sess-1", "tool_name": "Read" }),
    );
    assert_eq!(
        pre_tool.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: None,
            turn_id: None,
        }
    );
}

#[test]
fn tool_signals_carry_the_call_id_as_the_native_key() {
    fn native_key(signal: &LifecycleSignal) -> Option<&str> {
        match signal {
            LifecycleSignal::AwaitingInput { native_key, .. }
            | LifecycleSignal::ToolUsed { native_key, .. } => native_key.as_deref(),
            other => panic!("expected an ask or tool signal, got {other:?}"),
        }
    }

    for (event, payload, expected) in [
        // One call's Pre and Post share the id, so the ask and its completion
        // edge carry the same key.
        (
            "PreToolUse",
            json!({
                "session_id": "sess-1",
                "tool_name": "AskUserQuestion",
                "tool_use_id": "toolu_ask",
                "tool_input": {},
            }),
            Some("toolu_ask"),
        ),
        (
            "PostToolUse",
            json!({
                "session_id": "sess-1",
                "tool_name": "AskUserQuestion",
                "tool_use_id": "toolu_ask",
            }),
            Some("toolu_ask"),
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-1",
                "tool_name": "ExitPlanMode",
                "tool_use_id": "toolu_plan",
                "tool_input": {},
            }),
            Some("toolu_plan"),
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-1",
                "tool_name": "Read",
                "tool_use_id": "toolu_read",
            }),
            Some("toolu_read"),
        ),
        (
            "PostToolUse",
            json!({
                "session_id": "sess-1",
                "tool_name": "Read",
                "tool_use_id": "toolu_read",
            }),
            Some("toolu_read"),
        ),
        // A build that omits the id, or sends it empty, degrades to keyless.
        (
            "PostToolUse",
            json!({ "session_id": "sess-1", "tool_name": "Read" }),
            None,
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-1",
                "tool_name": "AskUserQuestion",
                "tool_use_id": "",
                "tool_input": {},
            }),
            None,
        ),
        // `PermissionRequest` has no id on the wire; a stray one never keys it,
        // because its clearing edge is the approved tool's own `PostToolUse`.
        (
            "PermissionRequest",
            json!({
                "session_id": "sess-1",
                "tool_name": "Bash",
                "tool_use_id": "toolu_stray",
            }),
            None,
        ),
    ] {
        let observed = hook_lifecycle(&ClaudeAdapter, event, &payload);
        assert_eq!(native_key(&observed.signal), expected, "{event} {payload}");
    }
}

#[test]
fn session_start_stop_background_and_end_events_map_to_rollup_signals() {
    for (source, expected_signal, expected_origin) in [
        (
            "compact",
            LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
            },
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
        (
            "fork",
            LifecycleSignal::Registered,
            Some(SessionOrigin::Forked),
        ),
        ("future", LifecycleSignal::Registered, None),
    ] {
        let obs = hook_lifecycle(
            &ClaudeAdapter,
            "SessionStart",
            &json!({ "session_id": "sess-1", "source": source }),
        );
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        assert_eq!(obs.signal, expected_signal, "{source}");
        assert_eq!(obs.origin, expected_origin, "{source}");
        assert_eq!(obs.task, None);
    }
    let absent = hook_lifecycle(
        &ClaudeAdapter,
        "SessionStart",
        &json!({ "session_id": "sess-1" }),
    );
    assert_eq!(absent.signal, LifecycleSignal::Registered);
    assert_eq!(absent.origin, Some(SessionOrigin::Fresh));

    assert!(hook_observation(&ClaudeAdapter, "Notification", &json!({})).is_none());

    for (case, payload, expected_signal) in [
        (
            "clean stop",
            json!({ "session_id": "sess-1" }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
        ),
        (
            "errored stop",
            json!({ "session_id": "sess-1", "is_error": true }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: None,
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
                turn_id: None,
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
                turn_id: None,
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
                turn_id: None,
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
                turn_id: None,
            },
        ),
    ] {
        let obs = hook_observation(&ClaudeAdapter, "Stop", &payload)
            .unwrap_or_else(|| panic!("{case} should produce a lifecycle observation"));
        assert_eq!(obs.signal, expected_signal, "{case}");
        assert_eq!(obs.task, None, "{case}");
    }

    let decoded = hook_output(
        &ClaudeAdapter,
        "SessionEnd",
        &json!({ "session_id": "sess-1" }),
    );
    let ended = decoded
        .lifecycle()
        .cloned()
        .expect("SessionEnd is a recorded lifecycle observation");
    assert_eq!(ended.agent_id.as_deref(), Some("sess-1"));
    assert!(decoded.ends_session());
    assert!(
        !hook_output(&ClaudeAdapter, "Stop", &json!({ "session_id": "sess-1" })).ends_session()
    );
}

#[test]
fn background_shell_reports_follow_launch_stop_and_notification() {
    fn shell_fields(shell: &BackgroundShell) -> (&str, Option<&str>, Option<&str>) {
        (
            shell.id.as_str(),
            shell.command.as_deref(),
            shell.description.as_deref(),
        )
    }
    let report = |event: &str, payload: serde_json::Value| {
        hook_observation(&ClaudeAdapter, event, &payload)
            .unwrap_or_else(|| panic!("{event} should produce a lifecycle observation"))
            .background_shells
    };

    let launch = |response: serde_json::Value| {
        report(
            "PostToolUse",
            json!({
                "session_id": "sess-1",
                "tool_name": "Bash",
                "tool_input": { "command": "cargo test", "description": "Run tests", "run_in_background": true },
                "tool_response": response,
            }),
        )
    };
    let Some(BackgroundShellReport::Started { shell }) =
        launch(json!({ "stdout": "", "backgroundTaskId": "b1" }))
    else {
        panic!("a Bash launch with a background task id starts a shell");
    };
    assert_eq!(
        shell_fields(&shell),
        ("b1", Some("cargo test"), Some("Run tests"))
    );
    assert_eq!(launch(json!({ "stdout": "ok" })), None);
    assert_eq!(
        report(
            "PostToolUse",
            json!({
                "session_id": "sess-1",
                "agent_id": "child-1",
                "tool_name": "Bash",
                "tool_response": { "backgroundTaskId": "b9" },
            }),
        ),
        None,
        "a subagent's launch targets its child row, which lists no shells"
    );

    let Some(BackgroundShellReport::Snapshot { shells }) = report(
        "Stop",
        json!({
            "session_id": "sess-1",
            "background_tasks": [
                { "id": "b1", "type": "shell", "status": "running", "command": "cargo test", "description": "Run tests" },
                { "id": "b2", "type": "shell", "status": "completed", "command": "ls" },
                { "id": "a1", "type": "subagent", "status": "running", "description": "Explore" },
                { "id": "b3", "status": "running", "command": "sleep 60" },
                { "id": "m1", "status": "running", "description": "no command" },
            ]
        }),
    ) else {
        panic!("a Stop task list snapshots the shells");
    };
    assert_eq!(
        shells.iter().map(shell_fields).collect::<Vec<_>>(),
        vec![
            ("b1", Some("cargo test"), Some("Run tests")),
            ("b3", Some("sleep 60"), None),
        ]
    );
    assert_eq!(
        report(
            "Stop",
            json!({ "session_id": "sess-1", "background_tasks": [] })
        ),
        Some(BackgroundShellReport::Snapshot { shells: Vec::new() })
    );
    assert_eq!(
        report("Stop", json!({ "session_id": "sess-1" })),
        None,
        "a build without the task list proves nothing about shells"
    );

    let notification = hook_observation(
        &ClaudeAdapter,
        "UserPromptSubmit",
        &json!({
            "session_id": "sess-1",
            "prompt": "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>done</summary>\n</task-notification>",
        }),
    )
    .expect("a notification prompt is a lifecycle observation");
    assert_eq!(
        notification.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
    assert_eq!(
        notification.background_shells,
        Some(BackgroundShellReport::Finished {
            ids: vec!["b1".to_owned()]
        })
    );
    assert_eq!(
        report(
            "UserPromptSubmit",
            json!({ "session_id": "sess-1", "prompt": "<task-notification><task-id>b1</task-id>" }),
        ),
        None
    );
}

#[test]
fn root_registration_stamps_birth_account_key_only_on_registration() {
    for source in ["startup", "resume"] {
        let payload = json!({ "session_id": "sess-1", "source": source });
        let parts = ClaudeLifecycleParts::parse("SessionStart", &payload);
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::Registered,
        );
        enrich_root_registration(&mut observation, &parts, || Some("fixture-key".to_owned()));
        assert_eq!(observation.account_key.as_deref(), Some("fixture-key"));
    }

    let compact_payload = json!({ "session_id": "sess-1", "source": "compact" });
    let compact_parts = ClaudeLifecycleParts::parse("SessionStart", &compact_payload);
    let mut compact = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: false,
        },
    );
    enrich_root_registration(&mut compact, &compact_parts, || {
        Some("fixture-key".to_owned())
    });
    assert_eq!(compact.account_key, None);

    let startup_payload = json!({ "session_id": "child", "source": "startup" });
    let startup_parts = ClaudeLifecycleParts::parse("SessionStart", &startup_payload);
    let mut subagent = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("child")),
        LifecycleSignal::Registered,
    );
    subagent.parent_agent_id = Some(AgentSessionId::from("parent"));
    enrich_root_registration(&mut subagent, &startup_parts, || {
        Some("fixture-key".to_owned())
    });
    assert_eq!(subagent.account_key, None);
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
        let decoded = hook_output(&ClaudeAdapter, event, &payload);
        let obs = decoded
            .lifecycle()
            .cloned()
            .unwrap_or_else(|| panic!("{event} should be observed"));
        assert_eq!(
            decoded.ends_session(),
            matches!(obs.signal, LifecycleSignal::Ended),
            "{event} session-end predicate"
        );
    }
}
