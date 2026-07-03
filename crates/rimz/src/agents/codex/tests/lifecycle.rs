use super::*;
use crate::agents::SessionOrigin;

#[test]
fn observe_lifecycle_maps_each_event_to_its_signal() {
    // Each root event payload maps to its lifecycle signal; a payload that is
    // observed-but-silent (a non-mutating tool) yields no observation at all.
    // Identity and subagent boundaries are covered separately below.
    use LifecycleSignal::*;
    let cases: &[(&str, serde_json::Value, Option<LifecycleSignal>)] = &[
        (
            "SessionStart",
            json!({"session_id":"s","source":"compact"}),
            Some(CompactionEnded { auto: None }),
        ),
        (
            "SessionStart",
            json!({"session_id":"s","source":"startup"}),
            Some(Registered),
        ),
        (
            "SessionStart",
            json!({"session_id":"s","source":"resume"}),
            Some(Registered),
        ),
        (
            "SessionStart",
            json!({"session_id":"s","source":"clear"}),
            Some(Registered),
        ),
        (
            "SessionStart",
            json!({"session_id":"s","source":"future"}),
            Some(Registered),
        ),
        (
            "PreCompact",
            json!({"session_id":"s","trigger":"manual"}),
            Some(Compacting),
        ),
        (
            "PostCompact",
            json!({"session_id":"s","trigger":"auto"}),
            Some(CompactionEnded { auto: Some(true) }),
        ),
        (
            "PostCompact",
            json!({"session_id":"s","trigger":"manual"}),
            Some(CompactionEnded { auto: Some(false) }),
        ),
        (
            "PostCompact",
            json!({"session_id":"s","trigger":"future"}),
            Some(CompactionEnded { auto: None }),
        ),
        (
            "PostCompact",
            json!({"session_id":"s"}),
            Some(CompactionEnded { auto: None }),
        ),
        (
            "PreToolUse",
            json!({"session_id":"s","tool_name":"shell"}),
            Some(ToolUsed {
                mutates: false,
                edits: false,
            }),
        ),
        (
            "PostToolUse",
            json!({"session_id":"s","tool_name":"shell"}),
            Some(ToolUsed {
                mutates: true,
                edits: false,
            }),
        ),
        (
            "PostToolUse",
            json!({"session_id":"s","tool_name":"read"}),
            None,
        ),
        (
            "Stop",
            json!({"session_id":"s"}),
            Some(TurnEnded {
                errored: false,
                parked_on_background: false,
            }),
        ),
        (
            "Stop",
            json!({"session_id":"s","status":"failed"}),
            Some(TurnEnded {
                errored: true,
                parked_on_background: false,
            }),
        ),
    ];
    for (event, payload, expected) in cases {
        let signal = CodexAdapter
            .observe_lifecycle(event, payload)
            .map(|obs| obs.signal);
        assert_eq!(&signal, expected, "{event} {payload}");
    }

    // A SessionStart carries the session as the agent identity and no task.
    let session = CodexAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({"session_id":"sess-1","source":"startup"}),
        )
        .unwrap();
    assert_eq!(session.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(session.task, None);
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
fn root_identity_events_stamp_codex_session_origin() {
    let dir = tempfile::tempdir().unwrap();
    let day_dir = dir.path().join("2026").join("06").join("26");
    std::fs::create_dir_all(&day_dir).unwrap();
    let write_rollout = |session_id: &str, head: &str| {
        std::fs::write(
            day_dir.join(format!("rollout-2026-06-26T00-00-00-{session_id}.jsonl")),
            format!("{head}\n"),
        )
        .unwrap();
    };
    write_rollout(
        "fresh",
        r#"{"type":"session_meta","payload":{"id":"fresh"}}"#,
    );
    write_rollout(
        "fork",
        r#"{"type":"session_meta","payload":{"id":"fork","forked_from_id":"fresh"}}"#,
    );

    with_codex_sessions_root(dir.path(), || {
        let registered = CodexAdapter
            .observe_lifecycle(
                "SessionStart",
                &json!({"session_id":"fresh","source":"startup"}),
            )
            .unwrap();
        assert_eq!(registered.origin, Some(SessionOrigin::Fresh));

        let turn_started = CodexAdapter
            .observe_lifecycle(
                "UserPromptSubmit",
                &json!({"session_id":"fork","prompt":"continue"}),
            )
            .unwrap();
        assert_eq!(turn_started.origin, Some(SessionOrigin::Forked));

        let stop = CodexAdapter
            .observe_lifecycle("Stop", &json!({"session_id":"fresh"}))
            .unwrap();
        assert_eq!(stop.origin, None);
    });
}

#[test]
fn root_lifecycle_effort_falls_back_to_rollout_turn_context() {
    let dir = tempfile::tempdir().unwrap();
    let day_dir = dir.path().join("2026").join("06").join("26");
    std::fs::create_dir_all(&day_dir).unwrap();
    std::fs::write(
        day_dir.join("rollout-2026-06-26T00-00-00-sess-live.jsonl"),
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\",\"effort\":\"xhigh\"}}\n",
    )
    .unwrap();

    let turn_started = with_codex_sessions_root(dir.path(), || {
        CodexAdapter
            .observe_lifecycle(
                "UserPromptSubmit",
                &json!({"session_id":"sess-live","prompt":"continue"}),
            )
            .unwrap()
    });

    assert_eq!(turn_started.launch.model.as_deref(), Some("gpt-5"));
    assert_eq!(turn_started.launch.effort.as_deref(), Some("xhigh"));
}

#[test]
fn subagent_lifecycle_effort_falls_back_to_codex_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
model_reasoning_effort = "xhigh"
"#,
    )
    .unwrap();

    let start = with_codex_config_path(&path, || {
        CodexAdapter
            .observe_lifecycle(
                "SubagentStart",
                &json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-thread-1",
                    "agent_type": "review",
                }),
            )
            .unwrap()
    });

    assert_eq!(start.parent_agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(start.launch.effort.as_deref(), Some("xhigh"));
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
        server_url: None,
    };
    let spawn = CodexAdapter
        .context_refresh_spawn(crate::agents::RefreshTrigger::Hook("Stop"), &ctx)
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
    let tick_spawn = CodexAdapter
        .context_refresh_spawn(crate::agents::RefreshTrigger::Tick, &ctx)
        .expect("producer tick refreshes");
    assert_eq!(tick_spawn.args, spawn.args);
    let bare = crate::agents::LifecycleRefreshCtx {
        model_hint: None,
        ..ctx
    };
    assert!(
        !CodexAdapter
            .context_refresh_spawn(crate::agents::RefreshTrigger::Hook("SessionStart"), &bare)
            .unwrap()
            .args
            .iter()
            .any(|arg| arg == "--model")
    );
    for event in ["PreToolUse", "PostToolUse", "SubagentStop", "Notification"] {
        assert!(
            CodexAdapter
                .context_refresh_spawn(crate::agents::RefreshTrigger::Hook(event), &ctx)
                .is_none(),
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
        prior_transcript_path: Some(&path),
        prior_transcript_stat: None,
    };
    let refresh = CodexAdapter
        .local_context_refresh(crate::agents::RefreshTrigger::Hook("PostToolUse"), &ctx)
        .expect("PostToolUse reads local transcript context");
    // The refresh carries the window and current-usage breakdown; the gauge
    // derives the percentage (500 of 1000 = 50%) downstream rather than baking
    // it into the sidecar.
    assert_eq!(
        refresh
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.context_window_size),
        Some(1000)
    );
    assert!(
        CodexAdapter
            .local_context_refresh(crate::agents::RefreshTrigger::Hook("PreToolUse"), &ctx)
            .is_none()
    );
    assert!(
        CodexAdapter
            .local_context_refresh(
                crate::agents::RefreshTrigger::Hook("PermissionRequest"),
                &ctx
            )
            .is_none()
    );

    use crate::agents::ClaudeAdapter;
    assert!(CodexAdapter.descriptor().hook_cap < ClaudeAdapter.descriptor().hook_cap);
}
