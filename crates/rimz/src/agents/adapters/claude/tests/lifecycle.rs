use super::*;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output};
use crate::ids::AgentSessionId;

#[test]
fn taken_question_summary_uses_first_line_and_extra_count() {
    let mut output = hook_output(
        &ClaudeAdapter,
        "PreToolUse",
        &json!({
            "session_id": "sess-1",
            "tool_name": "AskUserQuestion",
            "tool_input": {"questions": [
                {"question": "Which route?\nMore context", "options": []},
                {"question": "When?", "options": []}
            ]}
        }),
    );
    let LifecycleSignal::AwaitingInput {
        detail,
        extra_questions,
        ..
    } = output.take_lifecycle().unwrap().signal
    else {
        panic!("question wait")
    };
    assert_eq!(
        (detail.as_deref(), extra_questions),
        (Some("Which route?"), 1)
    );
}

#[test]
fn stop_failure_lifecycle_matches_the_error_class() {
    use TurnErrorClass::{
        Failed, NotEntitled, PausedOverloaded, PausedRateLimit, PausedSpendLimit,
    };

    let cases = [
        ("api_error", "API Error: Bad Request", Failed, true),
        ("unclassified", "API Error: Bad Request", Failed, true),
        ("billing_error", "API Error: Bad Request", NotEntitled, true),
        (
            "rate_limit",
            "API Error: Bad Request",
            PausedRateLimit,
            false,
        ),
        (
            "overloaded",
            "API Error: Bad Request",
            PausedOverloaded,
            false,
        ),
        (
            "server_error",
            "API Error: Bad Request",
            PausedOverloaded,
            false,
        ),
        (
            "rate_limit",
            "You've hit your monthly spend limit.",
            PausedSpendLimit,
            false,
        ),
        (
            "api_error",
            "API Error: Server Error",
            PausedOverloaded,
            false,
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(error, label, _, _)| {
            let output = hook_output(
                &ClaudeAdapter,
                "StopFailure",
                &json!({
                    "session_id": "sess-1",
                    "error": error,
                    "last_assistant_message": label
                }),
            );
            (
                output.turn_error().expect("error marker").class,
                output
                    .lifecycle()
                    .map(|observation| observation.signal.clone()),
            )
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, _, class, terminal)| {
            (
                *class,
                terminal.then_some(LifecycleSignal::TurnEnded {
                    errored: true,
                    parked_on_background: false,
                    turn_id: None,
                }),
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn stop_failure_without_an_error_has_no_lifecycle_or_marker() {
    for error in [None, Some(""), Some(" \t ")] {
        let output = hook_output(
            &ClaudeAdapter,
            "StopFailure",
            &json!({
                "session_id": "sess-1",
                "error": error,
                "last_assistant_message": "API Error: Bad Request"
            }),
        );
        assert!(output.turn_error().is_none());
        assert!(output.lifecycle().is_none());
    }
}

#[test]
fn stop_failure_never_uses_the_provider_label_as_a_final_message() {
    for error in ["api_error", "billing_error", "rate_limit"] {
        let output = hook_output(
            &ClaudeAdapter,
            "StopFailure",
            &json!({
                "session_id": "sess-1",
                "error": error,
                "last_assistant_message": "API Error: Bad Request"
            }),
        );
        assert_eq!(output.final_message(), None, "{error}");
    }
}

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
fn cursor_quarantine_keeps_blocking_tools_out_of_the_catalog_ask() {
    let mut payload = json!({
        "cursor_version": "1.7.0",
        "session_id": "sess-1",
        "tool_name": "AskUserQuestion",
        "tool_use_id": "toolu_q"
    });
    let output = hook_output(&ClaudeAdapter, "PreToolUse", &payload);
    assert_eq!(output.class(), AgentHookClass::Unknown);
    assert_eq!(output.ask_kind(), None);
    assert!(matches!(
        output.lifecycle().map(|observation| &observation.signal),
        Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            native_key: Some(key),
            ..
        }) if key == "toolu_q"
    ));

    payload.as_object_mut().unwrap().remove("cursor_version");
    let output = hook_output(&ClaudeAdapter, "PreToolUse", &payload);
    assert_eq!(output.class(), AgentHookClass::AwaitingUser);
    assert_eq!(output.ask_kind(), Some(AskKind::Question));
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
            effect: ToolEffect::Observed,
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
                echo: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "trigger": "manual" }),
            LifecycleSignal::CompactionEnded {
                auto: Some(false),
                failed: false,
                echo: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "trigger": "future" }),
            LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
                echo: false,
            },
        ),
        (
            json!({ "session_id": "sess-1" }),
            LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
                echo: false,
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
                effect: ToolEffect::Editing,
                name: Some("Edit".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "Bash",
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Mutating,
                name: Some("Bash".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "Read",
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Observed,
                name: Some("Read".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "AskUserQuestion",
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Observed,
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
            effect: ToolEffect::Observed,
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
                echo: true,
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

/// The dispatch-mode capture, in hook order: a quick script settles inside its
/// turn, a long one outlives it and posts its result after that turn's `Stop`,
/// and the settle starts a turn whose `Stop` lists no task.
#[test]
fn background_script_reports_replay_the_captured_repl_sequence() {
    let report = |event: &str, payload: serde_json::Value| {
        let mut payload = payload;
        payload["session_id"] = json!("sess-1");
        payload["hook_event_name"] = json!(event);
        let mut report =
            hook_observation(&ClaudeAdapter, event, &payload).and_then(|o| o.background_scripts);
        if let Some(BackgroundScriptReport::Started { script }) = &mut report {
            script.started_at = Timestamp::UNIX_EPOCH;
        }
        report
    };
    let started = |id: &str, description: &str| {
        Some(BackgroundScriptReport::Started {
            script: BackgroundScript {
                id: id.to_owned(),
                description: Some(description.to_owned()),
                started_at: Timestamp::UNIX_EPOCH,
                held_by: None,
            },
        })
    };
    let finished = |id: &str| Some(BackgroundScriptReport::Finished { id: id.to_owned() });
    let quick = json!({ "code": "'quick'", "description": "quick one" });
    let long = json!({ "code": "await $`sleep 75`; 'slept'", "description": "long sleep" });
    let inner = json!({ "command": "sleep 75" });
    let running = json!([{ "id": "xhc7erb82", "type": "code", "status": "running", "description": "long sleep" }]);
    let replay = [
        (
            "PreToolUse",
            json!({ "tool_name": "REPL", "tool_input": quick, "tool_use_id": "toolu_01F3" }),
            started("toolu_01F3", "quick one"),
        ),
        (
            "PostToolUse",
            json!({
                "tool_name": "REPL", "tool_input": quick, "tool_use_id": "toolu_01F3", "duration_ms": 45,
                "tool_response": { "code": "'quick'", "result": "quick", "stdout": "", "stderr": "" },
            }),
            finished("toolu_01F3"),
        ),
        (
            "PreToolUse",
            json!({ "tool_name": "REPL", "tool_input": long, "tool_use_id": "toolu_011h" }),
            started("toolu_011h", "long sleep"),
        ),
        (
            "PreToolUse",
            json!({ "tool_name": "Bash", "tool_input": inner, "tool_use_id": "repl_0a5b" }),
            None,
        ),
        (
            "PreToolUse",
            json!({ "tool_name": "Poll", "tool_input": {}, "tool_use_id": "toolu_01Sm" }),
            None,
        ),
        (
            "PostToolUse",
            json!({
                "tool_name": "Poll", "tool_input": {}, "tool_use_id": "toolu_01Sm", "duration_ms": 3,
                "tool_response": { "content": "<event kind=\"repl-eval\" eval=\"1\" status=\"settled\">", "eventCount": 1 },
            }),
            None,
        ),
        (
            "Stop",
            json!({ "stop_hook_active": false, "background_tasks": running }),
            None,
        ),
        (
            "SubagentStop",
            json!({ "agent_id": "a1383169d30d17917", "agent_type": "", "stop_hook_active": false, "background_tasks": running }),
            None,
        ),
        (
            "PostToolUse",
            json!({
                "tool_name": "Bash", "tool_input": inner, "tool_use_id": "repl_0a5b", "duration_ms": 79559,
                "tool_response": { "stdout": "", "stderr": "", "interrupted": false },
            }),
            None,
        ),
        (
            "PostToolUse",
            json!({
                "tool_name": "REPL", "tool_input": long, "tool_use_id": "toolu_011h", "duration_ms": 80307,
                "tool_response": { "code": "await", "result": "slept", "stdout": "", "stderr": "" },
            }),
            finished("toolu_011h"),
        ),
        (
            "Stop",
            json!({ "stop_hook_active": false, "background_tasks": [] }),
            Some(BackgroundScriptReport::NoneRunning),
        ),
        (
            "SubagentStop",
            json!({ "agent_id": "ad8bcdc3a3eb4bfa6", "agent_type": "", "stop_hook_active": false, "background_tasks": [] }),
            None,
        ),
    ];
    for (index, (event, payload, expected)) in replay.into_iter().enumerate() {
        assert_eq!(report(event, payload), expected, "record {index}: {event}");
    }

    assert_eq!(
        report("Stop", json!({ "stop_hook_active": false })),
        None,
        "a build without the task list proves nothing about scripts"
    );
    assert_eq!(
        report(
            "Stop",
            json!({ "background_tasks": [
                { "id": "b1", "type": "shell", "status": "running", "command": "sleep 60" },
                { "id": "x1", "type": "code", "status": "completed", "description": "done" },
            ] }),
        ),
        Some(BackgroundScriptReport::NoneRunning),
        "only a pending code task keeps the scripts"
    );
    assert_eq!(
        report(
            "PreToolUse",
            json!({ "agent_id": "child-1", "tool_name": "REPL", "tool_input": long, "tool_use_id": "toolu_child" }),
        ),
        None,
        "a subagent's script targets its child row, which lists no scripts"
    );

    // A script blocked on its own AskUserQuestion: the ask reports no script
    // change and flags itself as raised inside a script.
    let asking =
        json!({ "code": "await tools.AskUserQuestion({})", "description": "ask the user" });
    let question = json!({ "questions": [{ "question": "Ship it?", "options": [] }] });
    let script_ask = |tool_name: &str, tool_input: &serde_json::Value, tool_use_id: &str| {
        let payload = json!({
            "session_id": "sess-1", "hook_event_name": "PreToolUse",
            "tool_name": tool_name, "tool_input": tool_input, "tool_use_id": tool_use_id,
        });
        hook_observation(&ClaudeAdapter, "PreToolUse", &payload)
            .map(|o| (o.script_ask, o.background_scripts.is_some()))
    };
    assert_eq!(
        script_ask("REPL", &asking, "toolu_01BS"),
        Some((false, true))
    );
    assert_eq!(
        script_ask("AskUserQuestion", &question, "repl_e3d51258"),
        Some((true, false)),
        "an ask a script raised holds that script"
    );
    assert_eq!(
        script_ask("AskUserQuestion", &question, "toolu_01Ask"),
        Some((false, false)),
        "a direct ask holds no script"
    );
    assert_eq!(
        script_ask("Bash", &json!({ "command": "true" }), "repl_0a5b"),
        Some((false, false)),
        "a script's own command is no ask"
    );
}

#[test]
fn root_registration_stamps_birth_account_key_only_on_registration() {
    for source in ["startup", "resume"] {
        let payload = json!({ "session_id": "sess-1", "source": source });
        let hook = ClaudeHook::parse("SessionStart", &payload);
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::Registered,
        );
        enrich_root_registration(&mut observation, &hook, || Some("fixture-key".to_owned()));
        assert_eq!(observation.account_key.as_deref(), Some("fixture-key"));
    }

    let compact_payload = json!({ "session_id": "sess-1", "source": "compact" });
    let compact_hook = ClaudeHook::parse("SessionStart", &compact_payload);
    let mut compact = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: false,
            echo: false,
        },
    );
    enrich_root_registration(&mut compact, &compact_hook, || {
        Some("fixture-key".to_owned())
    });
    assert_eq!(compact.account_key, None);

    let startup_payload = json!({ "session_id": "child", "source": "startup" });
    let startup_hook = ClaudeHook::parse("SessionStart", &startup_payload);
    let mut subagent = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("child")),
        LifecycleSignal::Registered,
    );
    subagent.parent_agent_id = Some(AgentSessionId::from("parent"));
    enrich_root_registration(&mut subagent, &startup_hook, || {
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

#[test]
fn effort_reads_the_stop_payloads_and_falls_back_to_thinking_level() {
    let effort = |event: &str, payload: serde_json::Value| {
        hook_lifecycle(&ClaudeAdapter, event, &payload)
            .launch
            .effort
    };
    let marked = |extra: serde_json::Value| {
        let mut payload = json!({
            "session_id": "sess-parent",
            "effort": {"level": "high"},
            "thinking_level": "low",
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        payload
    };
    assert_eq!(effort("Stop", marked(json!({}))).as_deref(), Some("high"));
    assert_eq!(
        effort("SubagentStop", marked(json!({"agent_id": "child-1"}))).as_deref(),
        Some("high")
    );
    assert_eq!(
        effort("PostToolUse", marked(json!({"tool_name": "Read"}))).as_deref(),
        Some("low"),
        "only the stop payloads carry a typed effort marker"
    );
}

fn claude_post_tool_use(tool_name: &str, tool_input: Value, tool_response: Option<Value>) -> Value {
    let mut payload = json!({
        "session_id": "sess-1",
        "transcript_path": "/nonexistent/sess-1.jsonl",
        "cwd": "/repo",
        "permission_mode": "default",
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input,
        "tool_use_id": "toolu_01",
        "duration_ms": 12,
        "effort": {"level": "high"},
    });
    if let Some(response) = tool_response {
        payload["tool_response"] = response;
    }
    payload
}

/// Synthetic values in the key shapes Claude sends: one payload per kept tool,
/// a presence-only tool, a large `Read` and `Edit`, `Bash` in its three
/// response shapes, and a response that is absent and one that is null.
fn claude_guard_payloads() -> Vec<Value> {
    let body = "fn main() {}\n".repeat(2_000);
    let read = |response| {
        claude_post_tool_use("Read", json!({"file_path": "/repo/src/main.rs"}), response)
    };
    vec![
        claude_post_tool_use(
            "AskUserQuestion",
            json!({"questions": [{"question": "Which route?", "header": "Route", "options": [{"label": "safe"}, {"label": "fast"}], "multiSelect": false}]}),
            Some(
                json!({"questions": [{"question": "Which route?", "options": [{"label": "safe"}, {"label": "fast"}]}], "answers": {"Which route?": "safe"}}),
            ),
        ),
        claude_post_tool_use(
            "ExitPlanMode",
            json!({"plan": "1. Build it"}),
            Some(
                json!({"plan": "1. Build it", "isAgent": false, "filePath": "/home/u/.claude/plans/p.md"}),
            ),
        ),
        claude_post_tool_use(
            "Bash",
            json!({"command": "cargo test", "description": "Run tests", "run_in_background": true}),
            Some(
                json!({"stdout": "", "stderr": "", "interrupted": false, "isImage": false, "backgroundTaskId": "b1"}),
            ),
        ),
        claude_post_tool_use(
            "Bash",
            json!({"command": "cat src/main.rs"}),
            Some(json!({"stdout": body, "stderr": "", "interrupted": false, "isImage": false})),
        ),
        claude_post_tool_use("Bash", json!({"command": "true"}), Some(json!("ok"))),
        read(Some(
            json!({"type": "text", "file": {"filePath": "/repo/src/main.rs", "content": body, "numLines": 2_000, "startLine": 1, "totalLines": 2_000}}),
        )),
        claude_post_tool_use(
            "Edit",
            json!({"file_path": "/repo/src/main.rs", "old_string": "fn main() {}", "new_string": "fn main() { run() }", "replace_all": false}),
            Some(
                json!({"filePath": "/repo/src/main.rs", "oldString": "fn main() {}", "newString": "fn main() { run() }", "originalFile": body, "structuredPatch": [{"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-fn main() {}", "+fn main() { run() }"]}], "userModified": false, "replaceAll": false}),
            ),
        ),
        read(None),
        read(Some(Value::Null)),
    ]
}

#[test]
fn queued_post_tool_use_reads_as_the_received_one() {
    for payload in claude_guard_payloads() {
        crate::agents::testkit::assert_queued_post_tool_use_reads_alike(&ClaudeAdapter, &payload);
    }
}

#[test]
fn queued_post_tool_use_reads_alike_under_readers_without_the_keep_rule() {
    let reads = |payload: &Value| {
        let post: ClaudePostToolUse = decode_or_default(payload);
        let answers = post
            .tool_name
            .as_deref()
            .zip(post.tool_response.as_ref())
            .and_then(|(name, response)| ask::answer_detail(name, response));
        let shells =
            claude_background_shells(&ClaudeHook::PostToolUse(post), Timestamp::UNIX_EPOCH);
        (answers, shells)
    };
    for payload in claude_guard_payloads() {
        let queued = ClaudeAdapter
            .queued_hook_payload("PostToolUse", &payload)
            .unwrap_or_else(|| payload.clone());
        assert_eq!(reads(&payload), reads(&queued), "{payload}");
    }
}

#[test]
fn queued_post_tool_use_keeps_only_the_tool_output_rimz_reads() {
    let payloads = claude_guard_payloads();
    let responses: Vec<_> = payloads
        .iter()
        .map(|payload| {
            let queued = ClaudeAdapter.queued_hook_payload("PostToolUse", payload)?;
            let without_response = |payload: &Value| {
                let mut payload = payload.clone();
                payload.as_object_mut().unwrap().remove("tool_response");
                payload
            };
            assert_eq!(without_response(&queued), without_response(payload));
            Some(queued["tool_response"].clone())
        })
        .collect();
    assert_eq!(
        responses,
        [
            None,
            Some(json!({})),
            Some(json!({"backgroundTaskId": "b1"})),
            Some(json!({})),
            Some(json!({})),
            Some(json!({})),
            Some(json!({})),
            None,
            None,
        ]
    );
    let read = &payloads[5];
    for (event, payload) in [
        ("PreToolUse", read.clone()),
        ("Stop", read.clone()),
        ("PostToolUse", json!("not an object")),
        ("PostToolUse", Value::Null),
        ("PostToolUse", {
            let mut nameless = read.clone();
            nameless.as_object_mut().unwrap().remove("tool_name");
            nameless
        }),
    ] {
        assert_eq!(
            ClaudeAdapter.queued_hook_payload(event, &payload),
            None,
            "{event} {payload}"
        );
    }
}

#[test]
fn post_tool_use_decode_hands_readers_the_kept_tool_output() {
    let payloads = claude_guard_payloads();
    let ClaudeHook::PostToolUse(read) = ClaudeHook::parse("PostToolUse", &payloads[5]) else {
        panic!("PostToolUse decodes as itself");
    };
    assert_eq!(read.tool_response, Some(json!({})));
}
