use super::*;
use crate::agents::SessionOrigin;

#[test]
fn observe_lifecycle_maps_each_event_to_its_signal() {
    // Each root event payload maps to its lifecycle signal.
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
                native_key: None,
            }),
        ),
        (
            "PostToolUse",
            json!({"session_id":"s","tool_name":"shell"}),
            Some(ToolUsed {
                mutates: true,
                edits: false,
                native_key: None,
            }),
        ),
        (
            "PostToolUse",
            json!({"session_id":"s","tool_name":"Bash"}),
            Some(ToolUsed {
                mutates: true,
                edits: false,
                native_key: None,
            }),
        ),
        (
            "PostToolUse",
            json!({"session_id":"s","tool_name":"read"}),
            Some(ToolUsed {
                mutates: false,
                edits: false,
                native_key: None,
            }),
        ),
        (
            "PostToolUse",
            json!({"session_id":"s","tool_name":"request_user_input"}),
            Some(ToolUsed {
                mutates: false,
                edits: false,
                native_key: None,
            }),
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
            .decode_hook(event, payload)
            .expect("test hook decodes")
            .lifecycle()
            .map(|obs| obs.signal);
        assert_eq!(&signal, expected, "{event} {payload}");
    }

    // A SessionStart carries the session as the agent identity and no task.
    let session = CodexAdapter
        .decode_hook(
            "SessionStart",
            &json!({"session_id":"sess-1","source":"startup"}),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(session.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(session.task, None);
}

#[test]
fn stop_on_resting_plan_becomes_plan_approval_with_detail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-plan.jsonl");
    std::fs::write(
        &path,
        concat!(
            r##"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-plan","item":{"type":"Plan","text":"# Ship\n\nImplement it."}}}"##,
            "\n",
            r#"{"timestamp":"2026-07-13T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-plan","last_agent_message":"Codex says:"}}"#,
            "\n",
        ),
    )
    .unwrap();
    let payload = json!({
        "session_id": "sess-plan",
        "turn_id": "turn-plan",
        "transcript_path": path,
        "last_assistant_message": "Codex says:"
    });

    let observation = CodexAdapter
        .decode_hook("Stop", &payload)
        .expect("test hook decodes")
        .lifecycle()
        .expect("plan Stop observation");
    assert_eq!(
        observation.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::PlanApproval,
            ask_id: None,
            detail: None,
            native_key: None,
        }
    );
    let questions = CodexAdapter
        .decode_hook("Stop", &payload)
        .expect("test hook decodes")
        .questions();
    assert_eq!(
        questions[0].question,
        "Requesting plan approval:\n\n# Ship\n\nImplement it."
    );
    assert_eq!(questions[0].options[0].label, "implement");
}

#[test]
fn messageless_stop_keeps_raw_turn_error_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-messageless.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"timestamp":"2026-07-13T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-messageless","last_agent_message":null}}"#,
            "\n",
        ),
    )
    .unwrap();

    let observation = CodexAdapter
        .decode_hook(
            "Stop",
            &json!({
                "session_id": "sess-messageless",
                "turn_id": "turn-messageless",
                "transcript_path": path,
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .expect("messageless Stop observation");

    assert_eq!(
        observation.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
}

#[test]
fn root_and_child_lifecycle_events_keep_identity_boundaries() {
    let prompt = CodexAdapter
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

    let start = CodexAdapter
        .decode_hook(
            "SubagentStart",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(start.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(start.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(start.agent_name.as_deref(), Some("review"));
    assert_eq!(start.task.as_deref(), Some("review"));
    assert_eq!(start.launch.role.as_deref(), Some("review"));
    assert_eq!(start.parent_agent_id.as_deref(), Some("sess-parent"));

    let stop = CodexAdapter
        .decode_hook(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(stop.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(
        stop.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    assert_eq!(stop.task.as_deref(), Some("review"));
    assert_eq!(stop.parent_agent_id.as_deref(), Some("sess-parent"));

    let permission = CodexAdapter
        .decode_hook(
            "PermissionRequest",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
                "tool_name": "shell",
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(permission.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(
        permission.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ask_id: None,
            detail: None,
            native_key: None,
        }
    );
    assert_eq!(permission.task.as_deref(), Some("review"));
    assert_eq!(permission.parent_agent_id.as_deref(), Some("sess-parent"));

    let question = CodexAdapter
        .decode_hook(
            "PreToolUse",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
                "tool_name": "request_user_input",
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(question.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(
        question.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: None,
            native_key: None,
        }
    );
    assert_eq!(question.task.as_deref(), Some("review"));
    assert_eq!(question.parent_agent_id.as_deref(), Some("sess-parent"));

    for (event, payload) in [
        (
            "UserPromptSubmit",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
                "prompt": "continue",
            }),
        ),
        (
            "PostToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
                "tool_name": "shell",
            }),
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
                "tool_name": "shell",
            }),
        ),
        (
            "PreCompact",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
                "trigger": "auto",
            }),
        ),
        (
            "PostCompact",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
                "trigger": "auto",
            }),
        ),
    ] {
        let child = CodexAdapter
            .decode_hook(event, &payload)
            .expect("test hook decodes")
            .lifecycle()
            .unwrap_or_else(|| panic!("{event} should update the child"));
        assert_eq!(child.agent_id.as_deref(), Some("child-thread-1"), "{event}");
        assert_eq!(
            child.parent_agent_id.as_deref(),
            Some("sess-parent"),
            "{event}"
        );
        assert_eq!(child.task.as_deref(), Some("review"), "{event}");
    }

    let root = CodexAdapter
        .decode_hook(
            "PostToolUse",
            &json!({ "session_id": "sess-parent", "tool_name": "shell" }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(root.agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(root.parent_agent_id, None);
}

#[test]
fn v2_child_rollout_enriches_every_hook_without_parent_transcript_leakage() {
    let dir = tempfile::tempdir().unwrap();
    let day_dir = dir.path().join("2026").join("06").join("26");
    std::fs::create_dir_all(&day_dir).unwrap();
    let parent_path = day_dir.join("rollout-parent.jsonl");
    std::fs::write(
        &parent_path,
        concat!(
            r#"{"type":"session_meta","payload":{"id":"sess-parent","thread_source":"user"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"parent-model","effort":"low"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":999,"cached_input_tokens":0,"output_tokens":1,"total_tokens":1000},"model_context_window":2000}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let child_path = day_dir.join("rollout-child-thread-1.jsonl");
    std::fs::write(
        &child_path,
        concat!(
            r#"{"timestamp":"2026-06-26T00:00:00Z","type":"session_meta","payload":{"id":"child-thread-1","thread_source":"subagent","parent_thread_id":"nested-parent","agent_nickname":"Atlas","agent_path":"//root//research/explore_hooks/","agent_role":"explorer","multi_agent_version":"v2","source":{"subagent":{"thread_spawn":{"parent_thread_id":"nested-parent","depth":2}}}}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"child-model","effort":"xhigh"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":300,"cached_input_tokens":200,"output_tokens":21,"total_tokens":321},"total_token_usage":{"input_tokens":9999,"output_tokens":9999},"model_context_window":1000}}}"#,
            "\n",
            r#"{"timestamp":"2026-06-26T00:00:05Z","type":"event_msg","payload":{"type":"stream_error","message":"child failed"}}"#,
            "\n",
        ),
    )
    .unwrap();

    let start = with_codex_sessions_root(dir.path(), || {
        CodexAdapter
            .decode_hook(
                "SubagentStart",
                &json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-thread-1",
                    "agent_type": "default",
                    "transcript_path": parent_path,
                }),
            )
            .expect("test hook decodes")
            .lifecycle()
            .unwrap()
    });
    assert_eq!(start.parent_agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(start.agent_name.as_deref(), Some("Atlas"));
    assert_eq!(start.task.as_deref(), Some("research/explore_hooks"));
    assert_eq!(start.launch.role.as_deref(), Some("explorer"));
    assert_eq!(start.launch.model.as_deref(), Some("child-model"));
    assert_eq!(start.launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(start.total_tokens, Some(321));
    assert_eq!(start.context_window, Some(1000));
    assert_eq!(start.cache_read_input_tokens, Some(200));
    assert_eq!(start.fresh_input_tokens, Some(100));
    assert_eq!(start.output_tokens, Some(21));
    assert_eq!(start.transcript_path.as_deref(), child_path.to_str());

    let stop = CodexAdapter
        .decode_hook(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "default",
                "transcript_path": parent_path,
                "agent_transcript_path": child_path,
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(
        stop.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert_eq!(stop.launch.model.as_deref(), Some("child-model"));
    assert_ne!(stop.launch.model.as_deref(), Some("parent-model"));
    assert_eq!(stop.total_tokens, Some(321));
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
            .decode_hook(
                "SessionStart",
                &json!({"session_id":"fresh","source":"startup"}),
            )
            .expect("test hook decodes")
            .lifecycle()
            .unwrap();
        assert_eq!(registered.origin, Some(SessionOrigin::Fresh));

        let turn_started = CodexAdapter
            .decode_hook(
                "UserPromptSubmit",
                &json!({"session_id":"fork","prompt":"continue"}),
            )
            .expect("test hook decodes")
            .lifecycle()
            .unwrap();
        assert_eq!(turn_started.origin, Some(SessionOrigin::Forked));

        let stop = CodexAdapter
            .decode_hook("Stop", &json!({"session_id":"fresh"}))
            .expect("test hook decodes")
            .lifecycle()
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
            .decode_hook(
                "UserPromptSubmit",
                &json!({"session_id":"sess-live","prompt":"continue"}),
            )
            .expect("test hook decodes")
            .lifecycle()
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
            .decode_hook(
                "SubagentStart",
                &json!({
                    "session_id": "sess-parent",
                    "agent_id": "child-thread-1",
                    "agent_type": "review",
                }),
            )
            .expect("test hook decodes")
            .lifecycle()
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
        let decoded = CodexAdapter
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
    let pricing_cache_path = dir.path().join("pricing-cache.json");
    let ctx = crate::agents::LocalContextRefreshCtx {
        agent_id: "sess-1",
        model_hint: Some("gpt-5"),
        current_transcript_path: None,
        prior_transcript_path: Some(&path),
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing_cache_path,
    };
    let refresh = CodexAdapter
        .local_context_refresh(crate::agents::RefreshTrigger::Hook("PostToolUse"), &ctx)
        .expect("PostToolUse reads local transcript context");
    // The refresh carries the window and current-usage breakdown; the gauge
    // derives the percentage (500 of 1000 = 50%) downstream rather than baking
    // it into the sidecar.
    assert_eq!(
        refresh
            .context
            .tokens
            .as_value()
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
}
