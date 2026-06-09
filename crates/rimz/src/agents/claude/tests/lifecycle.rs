use super::*;

#[test]
fn classify_pretooluse_exit_plan_mode_is_plan_approval() {
    let c = ClaudeAdapter.classify_hook("PreToolUse", &json!({ "tool_name": "ExitPlanMode" }));
    assert_eq!(c.class, AgentHookClass::BlockingFeed);
    assert_eq!(c.feed_kind, Some(FeedKind::PlanApproval));
}

#[test]
fn classify_pretooluse_ask_user_question_is_question() {
    let c = ClaudeAdapter.classify_hook("PreToolUse", &json!({ "tool_name": "AskUserQuestion" }));
    assert_eq!(c.class, AgentHookClass::BlockingFeed);
    assert_eq!(c.feed_kind, Some(FeedKind::Question));
}

#[test]
fn classify_subagent_events_are_lifecycle() {
    for event in ["SubagentStart", "SubagentStop"] {
        let c = ClaudeAdapter.classify_hook(event, &json!({}));
        assert_eq!(c.class, AgentHookClass::Lifecycle, "{event}");
        assert_eq!(c.feed_kind, None, "{event}");
    }
}

#[test]
fn pre_compact_is_a_lifecycle_compaction_marker() {
    let c = ClaudeAdapter.classify_hook("PreCompact", &json!({ "session_id": "sess-1" }));
    assert_eq!(c.class, AgentHookClass::Lifecycle);
    assert_eq!(c.feed_kind, None);
    let obs = ClaudeAdapter
        .observe_lifecycle("PreCompact", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    // It carries the compaction signal; the reducer keeps the prior status
    // and only stamps the compacting head, never a false transition.
    assert_eq!(obs.signal, LifecycleSignal::Compacting);
}

#[test]
fn post_compact_maps_trigger_to_compaction_end() {
    let c = ClaudeAdapter.classify_hook("PostCompact", &json!({ "session_id": "sess-1" }));
    assert_eq!(c.class, AgentHookClass::Lifecycle);
    assert_eq!(c.feed_kind, None);

    let auto = ClaudeAdapter
        .observe_lifecycle(
            "PostCompact",
            &json!({ "session_id": "sess-1", "trigger": "auto" }),
        )
        .unwrap();
    assert_eq!(
        auto.signal,
        LifecycleSignal::CompactionEnded { auto: Some(true) }
    );

    for payload in [
        json!({ "session_id": "sess-1", "trigger": "manual" }),
        json!({ "session_id": "sess-1", "trigger": "future" }),
        json!({ "session_id": "sess-1" }),
    ] {
        let obs = ClaudeAdapter
            .observe_lifecycle("PostCompact", &payload)
            .unwrap();
        assert_eq!(
            obs.signal,
            LifecycleSignal::CompactionEnded { auto: Some(false) },
            "{payload}"
        );
    }
}

#[test]
fn subagent_start_observes_running_child_keyed_by_agent_id() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SubagentStart",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "subagent_type": "Explore",
                "description": "search the ledger",
                "permission_mode": "acceptEdits",
            }),
        )
        .unwrap();

    // Keyed off the child's own id, not the parent session.
    assert_eq!(obs.agent_id.as_deref(), Some("child-1"));
    assert_eq!(obs.signal, LifecycleSignal::SubagentStarted);
    // The type labels the child row; `session_id` is captured as the parent.
    assert_eq!(obs.task.as_deref(), Some("Explore"));
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn subagent_stop_clean_resolves_success_keeping_its_label() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "agent_type": "Explore",
            }),
        )
        .unwrap();

    assert_eq!(obs.agent_id.as_deref(), Some("child-1"));
    // No exit code reads as a clean finish.
    assert_eq!(
        obs.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    // The label persists past stop; the parent link survives.
    assert_eq!(obs.task.as_deref(), Some("Explore"));
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn subagent_stop_nonzero_exit_code_resolves_errored() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "agent_type": "Explore",
                "exit_code": 1,
            }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
}

#[test]
fn root_lifecycle_event_carries_no_parent() {
    let obs = ClaudeAdapter
        .observe_lifecycle("UserPromptSubmit", &json!({ "session_id": "sess-root" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-root"));
    assert_eq!(obs.parent_agent_id, None);
}

#[test]
fn subagent_event_without_child_id_is_quarantined() {
    // A SubagentStart that carries only the parent `session_id` (no distinct
    // child `agent_id`) must produce no observation — it can never fold onto
    // the parent's row and rename it to the subagent type. This is the
    // "main row becomes Explore" regression.
    let obs = ClaudeAdapter.observe_lifecycle(
        "SubagentStart",
        &json!({ "session_id": "sess-parent", "subagent_type": "Explore" }),
    );
    assert!(
        obs.is_none(),
        "a child with no distinct id is dropped, not folded onto the parent"
    );
}

#[test]
fn foreign_child_root_lifecycle_events_are_dropped() {
    // Claude stamps `agent_id` on payloads fired inside a subagent. Those
    // foreign-id root events must not fold onto the parent's rollup or mark its
    // turn/compaction state.
    for (event, payload, why) in [
        (
            "PostToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "tool_name": "Edit",
            }),
            "a backgrounded child's mutating tool must not fold onto the parent",
        ),
        (
            "PreCompact",
            json!({ "session_id": "sess-parent", "agent_id": "child-1" }),
            "a child's compaction never marks the parent",
        ),
        (
            "PostCompact",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "trigger": "auto",
            }),
            "a child's compaction end never marks the parent",
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "tool_name": "Read",
            }),
            "a child's pre-tool event never marks the parent",
        ),
    ] {
        let obs = ClaudeAdapter.observe_lifecycle(event, &payload);
        assert!(obs.is_none(), "{event}: {why}");
    }
}

#[test]
fn root_event_with_agent_id_equal_to_session_id_is_root() {
    // A session-equal `agent_id` is the main thread, not a child — a normal
    // root observation, never dropped.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({
                "session_id": "sess-1",
                "agent_id": "sess-1",
                "tool_name": "Edit",
            }),
        )
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(obs.parent_agent_id, None);
}

#[test]
fn harness_control_prompt_is_not_adopted_as_description() {
    // The harness injects synthetic user turns (a completed background task);
    // their raw text must never become the agent's description line.
    let obs = ClaudeAdapter
            .observe_lifecycle(
                "UserPromptSubmit",
                &json!({
                    "session_id": "sess-1",
                    "prompt": "<task-notification><task-id>afdc639e18e7ebdb9</task-id></task-notification>",
                }),
            )
            .unwrap();
    assert_eq!(obs.prompt, None, "control text is rejected, not shown");
    assert_eq!(obs.task, None);
}

#[test]
fn post_tool_use_rides_lifecycle_only_for_mutating_tools() {
    // A mutating tool proves real work, so it records a `ToolUsed` signal;
    // a read-only tool stays silent so the lifecycle channel isn't flooded.
    // A file edit also sets the `edits` bit (ends the thinking head); a
    // shell command mutates without editing.
    let edit = ClaudeAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "Edit" }),
        )
        .unwrap();
    assert_eq!(
        edit.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
        }
    );
    let shell = ClaudeAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "Bash" }),
        )
        .unwrap();
    assert_eq!(
        shell.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
        }
    );
    let read = ClaudeAdapter.observe_lifecycle(
        "PostToolUse",
        &json!({ "session_id": "sess-1", "tool_name": "Read" }),
    );
    assert!(read.is_none(), "a read-only tool stays silent");
}

#[test]
fn pre_tool_use_observes_proof_of_work() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "PreToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "Read" }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        }
    );
}

#[test]
fn hook_cap_is_120_seconds() {
    assert_eq!(
        ClaudeAdapter.descriptor().hook_cap,
        Duration::from_secs(120)
    );
}

#[test]
fn session_start_observes_idle_status() {
    let obs = ClaudeAdapter
        .observe_lifecycle("SessionStart", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    // Wired in, nothing asked yet — registered, no task.
    assert_eq!(obs.signal, LifecycleSignal::Registered);
    assert_eq!(obs.task, None);
}

#[test]
fn user_prompt_submit_observes_running_with_prompt_task() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "UserPromptSubmit",
            &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
        )
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(obs.signal, LifecycleSignal::TurnStarted);
    assert_eq!(obs.task.as_deref(), Some("fix auth flow"));
}

#[test]
fn todo_write_payload_extracts_progress() {
    // Claude TodoWrite hooks expose the todo list in `tool_input.todos`;
    // the reducer projects the count of completed items onto the row.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "UserPromptSubmit",
            &json!({
                "session_id": "sess-1",
                "tool_input": {
                    "todos": [
                        { "status": "completed" },
                        { "status": "completed" },
                        { "status": "in_progress" },
                        { "status": "pending" },
                    ]
                }
            }),
        )
        .unwrap();
    assert_eq!(obs.todo_done, Some(2));
    assert_eq!(obs.todo_total, Some(4));
}

#[test]
fn notification_event_is_not_a_lifecycle_observation() {
    let obs = ClaudeAdapter.observe_lifecycle("Notification", &json!({}));
    assert!(obs.is_none());
}

#[test]
fn stop_payload_variants_map_to_turn_end_signals() {
    // Claude Code v2.1.145+ reports in-flight `background_tasks` on Stop. A
    // pending task parks the turn, terminal tasks do not, and explicit errors
    // remain the attention signal even when the turn is also parked.
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
                    {
                        "id": "task-1",
                        "type": "command",
                        "command": "npm run build",
                        "status": "running",
                        "description": "Build process"
                    }
                ]
            }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: true,
            },
        ),
        (
            "multiple pending background tasks",
            json!({
                "session_id": "sess-1",
                "background_tasks": [
                    { "id": "a", "status": "running", "description": "lint" },
                    { "id": "b", "status": "running", "description": "test" }
                ]
            }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: true,
            },
        ),
        (
            "only completed background tasks",
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
            .observe_lifecycle("Stop", &payload)
            .unwrap_or_else(|| panic!("{case} should produce a lifecycle observation"));
        assert_eq!(obs.signal, expected_signal, "{case}");
        assert_eq!(
            obs.task, None,
            "{case} must not synthesize a background-task label"
        );
    }
}

#[test]
fn session_end_is_recorded_and_ends_the_session() {
    // SessionEnd must produce an observation so the reducer drops the agent
    // from the rollup, and must report `ends_session` so the CLI expires
    // the dead session's pending asks.
    let obs = ClaudeAdapter
        .observe_lifecycle("SessionEnd", &json!({ "session_id": "sess-1" }))
        .expect("SessionEnd is a recorded lifecycle observation");
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    assert!(ClaudeAdapter.ends_session("SessionEnd"));
    assert!(!ClaudeAdapter.ends_session("Stop"));
}

#[test]
fn turn_boundaries_move_the_session_on() {
    // Stop and a fresh prompt clear the session's mid-turn native_ui asks;
    // SessionStart/SessionEnd and tool events do not.
    assert!(ClaudeAdapter.moves_on("Stop"));
    assert!(ClaudeAdapter.moves_on("UserPromptSubmit"));
    assert!(!ClaudeAdapter.moves_on("SessionStart"));
    assert!(!ClaudeAdapter.moves_on("SessionEnd"));
    assert!(!ClaudeAdapter.moves_on("PostToolUse"));
}
