use super::*;

#[test]
fn session_sources_map_to_lifecycle_signals() {
    for (source, expected) in [
        ("compact", LifecycleSignal::CompactionEnded { auto: None }),
        ("startup", LifecycleSignal::Registered),
        ("resume", LifecycleSignal::Registered),
        ("clear", LifecycleSignal::Registered),
        ("future", LifecycleSignal::Registered),
    ] {
        let obs = CodexAdapter
            .observe_lifecycle(
                "SessionStart",
                &json!({ "session_id": "sess-1", "source": source }),
            )
            .unwrap();
        assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
        assert_eq!(obs.signal, expected, "{source}");
        assert_eq!(obs.task, None);
    }
}

#[test]
fn compaction_pair_maps_trigger_to_lifecycle_signals() {
    let pre = CodexAdapter
        .observe_lifecycle(
            "PreCompact",
            &json!({ "session_id": "sess-1", "trigger": "manual" }),
        )
        .unwrap();
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
        let obs = CodexAdapter
            .observe_lifecycle("PostCompact", &payload)
            .unwrap();
        assert_eq!(obs.signal, expected, "{payload}");
    }
}

#[test]
fn root_and_child_lifecycle_events_keep_identity_boundaries() {
    let prompt = CodexAdapter
        .observe_lifecycle(
            "UserPromptSubmit",
            &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
        )
        .unwrap();
    assert_eq!(prompt.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.task.as_deref(), Some("fix auth flow"));

    let start = CodexAdapter
        .observe_lifecycle(
            "SubagentStart",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .unwrap();
    assert_eq!(start.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(start.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(start.task.as_deref(), Some("review"));
    assert_eq!(start.parent_agent_id.as_deref(), Some("sess-parent"));

    let stop = CodexAdapter
        .observe_lifecycle(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .unwrap();
    assert_eq!(stop.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(
        stop.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    assert_eq!(stop.task.as_deref(), Some("review"));
    assert_eq!(stop.parent_agent_id.as_deref(), Some("sess-parent"));

    for (event, payload) in [
        (
            "PostToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "tool_name": "shell",
            }),
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "tool_name": "shell",
            }),
        ),
        (
            "PostCompact",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "trigger": "auto",
            }),
        ),
    ] {
        assert!(
            CodexAdapter.observe_lifecycle(event, &payload).is_none(),
            "{event}"
        );
    }

    let root = CodexAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-parent", "tool_name": "shell" }),
        )
        .unwrap();
    assert_eq!(root.agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(root.parent_agent_id, None);
}

#[test]
fn tool_and_stop_events_map_to_progress_signals() {
    let pre = CodexAdapter
        .observe_lifecycle(
            "PreToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "shell" }),
        )
        .unwrap();
    assert_eq!(
        pre.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        }
    );

    let mutating = CodexAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "shell" }),
        )
        .unwrap();
    assert_eq!(
        mutating.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
        }
    );
    assert!(
        CodexAdapter
            .observe_lifecycle(
                "PostToolUse",
                &json!({ "session_id": "sess-1", "tool_name": "read" }),
            )
            .is_none()
    );

    for (payload, expected) in [
        (
            json!({ "session_id": "sess-1" }),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "status": "failed" }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
    ] {
        let obs = CodexAdapter.observe_lifecycle("Stop", &payload).unwrap();
        assert_eq!(obs.signal, expected, "{payload}");
    }
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
        (
            "PostToolUse",
            json!({ "session_id": "sess-1", "tool_name": "shell" }),
        ),
        ("PreToolUse", json!({ "session_id": "sess-1" })),
        ("PreCompact", json!({ "session_id": "sess-1" })),
        ("PostCompact", json!({ "session_id": "sess-1" })),
    ] {
        let obs = CodexAdapter
            .observe_lifecycle(event, &payload)
            .unwrap_or_else(|| panic!("{event} should be observed"));
        assert_eq!(
            CodexAdapter.ends_session(event),
            matches!(obs.signal, LifecycleSignal::Ended),
            "{event} session-end predicate"
        );
        assert_eq!(
            CodexAdapter.moves_on(event),
            matches!(
                obs.signal,
                LifecycleSignal::TurnStarted | LifecycleSignal::TurnEnded { .. }
            ),
            "{event} moved-on predicate",
        );
    }
}

#[test]
fn codex_context_refreshes_are_bounded_to_turn_and_progress_events() {
    let ctx = crate::agents::LifecycleRefreshCtx {
        agent_id: "sess-1",
        workspace_id: "ws-1",
        model_hint: Some("gpt-5"),
    };
    let spawn = CodexAdapter
        .post_lifecycle_refresh("Stop", &ctx)
        .expect("Stop refreshes");
    assert_eq!(
        spawn.args,
        [
            "codex",
            "refresh-context",
            "--session-id",
            "sess-1",
            "--workspace-id",
            "ws-1",
            "--model",
            "gpt-5",
        ]
    );
    let bare = crate::agents::LifecycleRefreshCtx {
        model_hint: None,
        ..ctx
    };
    assert!(
        !CodexAdapter
            .post_lifecycle_refresh("SessionStart", &bare)
            .unwrap()
            .args
            .iter()
            .any(|arg| arg == "--model")
    );
    for event in ["PreToolUse", "PostToolUse", "SubagentStop", "Notification"] {
        assert!(
            CodexAdapter.post_lifecycle_refresh(event, &ctx).is_none(),
            "{event}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":300,\
             \"output_tokens\":20,\"total_tokens\":520},\
             \"model_context_window\":1000}}}\n",
    )
    .unwrap();
    let path = path.to_string_lossy().into_owned();
    let ctx = crate::agents::LocalContextRefreshCtx {
        agent_id: "sess-1",
        model_hint: Some("gpt-5"),
        prior_effort: None,
        prior_transcript_path: Some(&path),
        prior_transcript_stat: None,
    };
    let refresh = CodexAdapter
        .local_context_refresh("PostToolUse", &ctx)
        .expect("PostToolUse reads local transcript context");
    assert_eq!(
        refresh
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(50)
    );
    assert!(
        CodexAdapter
            .local_context_refresh("PreToolUse", &ctx)
            .is_none()
    );
    assert!(
        CodexAdapter
            .local_context_refresh("PermissionRequest", &ctx)
            .is_none()
    );

    use crate::agents::ClaudeAdapter;
    assert!(CodexAdapter.descriptor().hook_cap < ClaudeAdapter.descriptor().hook_cap);
}
