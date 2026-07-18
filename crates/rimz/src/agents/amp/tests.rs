use super::*;

use crate::agents::{
    AgentErr, AgentHookClass, AgentStatus, LaunchPreset, PriceBook, TurnPhase, step,
};
use serde_json::json;

#[test]
fn version_parser_keeps_the_amp_build_token_only() {
    assert_eq!(
        AmpAdapter
            .parse_version(
                "0.0.1783946745-g8c4c0a (released 2026-07-13T12:45:45.000Z)\n",
                "",
            )
            .as_deref(),
        Some("0.0.1783946745-g8c4c0a")
    );
    assert_eq!(
        AmpAdapter.parse_version("release 0.0.1783946745-g8c4c0a", ""),
        None
    );
    assert_eq!(
        AmpAdapter.parse_version("0.0.1-gnope (released today)", ""),
        None
    );
}

#[test]
fn launch_resume_and_preset_commands_match_amp_cli() {
    assert_eq!(
        AmpAdapter.launch_command(&[], None),
        Some(vec!["amp".to_owned()])
    );
    assert_eq!(
        AmpAdapter.launch_command(&[], Some("")),
        Some(vec!["amp".to_owned()])
    );
    assert_eq!(
        AmpAdapter.launch_command(&["--mode".to_owned(), "high".to_owned()], Some("fix auth")),
        Some(vec![
            "amp".to_owned(),
            "--mode".to_owned(),
            "high".to_owned(),
            "-x".to_owned(),
            "fix auth".to_owned(),
            "--plugin-ready-timeout".to_owned(),
            "30".to_owned(),
        ])
    );
    assert_eq!(
        AmpAdapter.resume_command("T-abc123", Path::new("/tmp")),
        Some(vec![
            "amp".to_owned(),
            "threads".to_owned(),
            "continue".to_owned(),
            "T-abc123".to_owned(),
        ])
    );
    assert_eq!(
        AmpAdapter.descriptor().launch.fork_command("T-abc123"),
        None
    );
    assert_eq!(AmpAdapter.descriptor().launch.compact_command(), None);

    assert_eq!(
        AmpAdapter.descriptor().render_preset(&LaunchPreset {
            model: Some("ultra".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--mode".to_owned(),
            "ultra".to_owned(),
            "--effort".to_owned(),
            "high".to_owned(),
        ])
    );
    assert!(matches!(
        AmpAdapter.descriptor().render_preset(&LaunchPreset {
            system_prompt_file: Some(PathBuf::from("/tmp/prompt")),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "amp",
            field: "system-prompt-file"
        })
    ));
}

#[test]
fn lifecycle_events_map_through_the_shared_state_machine() {
    let registered = AmpAdapter
        .decode_hook(
            "session_start",
            &json!({
                "session_id": "T-abc123",
                "cwd": "/tmp/repo",
                "model": "high",
                "effort": "xhigh"
            }),
        )
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    assert_eq!(registered.agent_id.as_deref(), Some("T-abc123"));
    assert_eq!(registered.worktree_path.as_deref(), Some("/tmp/repo"));
    assert_eq!(registered.launch.model.as_deref(), Some("high"));
    assert_eq!(registered.launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(registered.origin, Some(SessionOrigin::Fresh));
    let mut state = step(None, None, &registered.signal).next;
    assert_eq!(state.status, AgentStatus::Idle);

    let started = AmpAdapter
        .decode_hook(
            "agent_start",
            &json!({ "session_id": "T-abc123", "prompt": "  fix auth  " }),
        )
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    assert_eq!(started.prompt.as_deref(), Some("fix auth"));
    assert_eq!(started.task.as_deref(), Some("fix auth"));
    state = step(Some(&state), None, &started.signal).next;
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.phase, TurnPhase::Reasoning);

    let tool = AmpAdapter
        .decode_hook(
            "tool_result",
            &json!({
                "session_id": "T-abc123",
                "tool_name": "unknown_dynamic_tool",
                "files_modified": true,
                "status": "done"
            }),
        )
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    state = step(Some(&state), None, &tool.signal).next;
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.phase, TurnPhase::Acting);

    let waiting = AmpAdapter
        .decode_hook("permission_ask", &json!({ "session_id": "T-abc123" }))
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    state = step(Some(&state), None, &waiting.signal).next;
    assert_eq!(state.status, AgentStatus::Waiting);

    for (status, expected) in [
        ("done", AgentStatus::Success),
        ("error", AgentStatus::Failed),
        ("cancelled", AgentStatus::Failed),
    ] {
        let ended = AmpAdapter
            .decode_hook(
                "agent_end",
                &json!({ "session_id": "T-abc123", "status": status }),
            )
            .expect("test hook decodes")
            .lifecycle
            .unwrap();
        assert_eq!(
            step(Some(&state), None, &ended.signal).next.status,
            expected
        );
    }
}

#[test]
fn files_modified_flag_precedes_static_tool_fallback() {
    for (payload, edits) in [
        (
            json!({ "session_id": "T-abc123", "tool_name": "read", "files_modified": true }),
            true,
        ),
        (
            json!({ "session_id": "T-abc123", "tool_name": "apply_patch", "files_modified": false }),
            false,
        ),
        (
            json!({ "session_id": "T-abc123", "tool_name": "apply_patch" }),
            true,
        ),
    ] {
        let observed = AmpAdapter
            .decode_hook("tool_result", &payload)
            .expect("test hook decodes")
            .lifecycle
            .unwrap();
        assert_eq!(
            observed.signal,
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits,
                native_key: None,
            }
        );
    }
}

#[test]
fn agent_end_extracts_the_supervised_final_answer() {
    let payload = json!({
        "session_id": "T-abc123",
        "status": "done",
        "last_assistant_message": "  Fixed the race.  "
    });
    assert_eq!(
        AmpAdapter
            .decode_hook("agent_end", &payload)
            .expect("test hook decodes")
            .final_message
            .as_deref(),
        Some("Fixed the race.")
    );
    assert_eq!(
        AmpAdapter
            .decode_hook("tool_result", &payload)
            .expect("test hook decodes")
            .final_message,
        None
    );
}

#[test]
fn ask_classification_and_neutral_output_are_pinned() {
    let classified = AmpAdapter
        .decode_hook("permission_ask", &json!({ "session_id": "T-abc123" }))
        .expect("test hook decodes");
    assert_eq!(classified.class, AgentHookClass::AwaitingUser);
    assert_eq!(classified.ask_kind, Some(AskKind::Permission));
    insta::assert_snapshot!(
        format!("{:?}", AmpAdapter.decode_hook("permission_ask", &Value::Null).expect("test hook decodes").neutral),
        @"None"
    );
}

#[test]
fn malformed_payloads_fold_to_no_observation() {
    insta::assert_snapshot!(
        format!("{:?}", AmpAdapter.decode_hook("agent_start", &json!({ "prompt": "missing id" })).expect("test hook decodes").lifecycle),
        @"None"
    );
    insta::assert_snapshot!(
        format!("{:?}", AmpAdapter.decode_hook("agent_start", &json!("junk")).expect("test hook decodes").lifecycle),
        @"None"
    );
}

#[test]
fn install_preview_drift_and_uninstall_only_touch_managed_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins/rimz.ts");
    let events = || {
        AMP_HOOKS
            .iter()
            .map(|hook| hook.event.to_owned())
            .collect::<Vec<_>>()
    };

    let installed = AMP_MANAGED_SOURCE.install_into(&path).unwrap();
    assert!(!installed.files[0].existed);
    assert_eq!(installed.installed_events, events());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);
    assert!(AMP_MANAGED_SOURCE.installed_at(&path));

    std::fs::write(&path, "// stale _rimz_managed plugin\n").unwrap();
    let preview = AMP_MANAGED_SOURCE.preview_at(&path).unwrap();
    assert!(preview.files[0].existed);
    assert_eq!(preview.files[0].candidate, PLUGIN_SOURCE);
    AMP_MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);

    let removed = AMP_MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert_eq!(removed.removed_events, events());
    assert!(!path.exists());

    let user_path = dir.path().join("user.ts");
    std::fs::write(&user_path, "// user plugin\n").unwrap();
    assert!(matches!(
        AMP_MANAGED_SOURCE.install_into(&user_path),
        Err(AgentErr::Install { agent: "amp", .. })
    ));
    assert!(matches!(
        AMP_MANAGED_SOURCE.preview_at(&user_path),
        Err(AgentErr::Install { agent: "amp", .. })
    ));
    assert!(
        AMP_MANAGED_SOURCE
            .uninstall_from(&user_path)
            .unwrap()
            .removed_events
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// user plugin\n"
    );
}

#[test]
fn plugin_source_pins_active_thread_observation_wire_and_pid() {
    assert!(
        PLUGIN_SOURCE
            .lines()
            .next()
            .unwrap()
            .contains("_rimz_managed")
    );
    assert!(PLUGIN_SOURCE.contains("\"hooks\", \"feed\", \"--source\", \"amp\""));
    assert!(PLUGIN_SOURCE.contains("RIMZ_AGENT_PID"));
    assert!(PLUGIN_SOURCE.contains("amp.activeThread.current"));
    assert!(PLUGIN_SOURCE.contains("awaiting-approval"));
    assert!(PLUGIN_SOURCE.contains("filesModifiedByToolCall"));
    assert!(PLUGIN_SOURCE.contains("sendQueue = sendQueue.then"));
    assert!(PLUGIN_SOURCE.contains("lastAssistantMessage(event.messages)"));
    assert!(PLUGIN_SOURCE.contains("files == null ? undefined"));
    assert!(!PLUGIN_SOURCE.contains("amp.on(\"tool.call\""));
    for event in ["session.start", "agent.start", "tool.result", "agent.end"] {
        assert!(PLUGIN_SOURCE.contains(event), "plugin missing {event}");
    }
}

#[test]
fn rewritten_cache_streams_completed_assistant_messages_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("T-stream.json");
    let path_text = path.to_string_lossy().into_owned();
    std::fs::write(
        &path,
        r#"{"id":"T-stream","messages":[{"role":"assistant","messageId":"a","content":"old","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","outputTokens":1}}]}"#,
    )
    .unwrap();

    let mut attached = crate::agents::transcript::TranscriptCursor::new(false);
    let session_id = AgentSessionId::from("T-stream");
    assert!(
        attached
            .messages(Some(&path_text), Some(&session_id), &AmpAdapter)
            .is_empty()
    );

    std::fs::write(
        &path,
        r#"{"id":"T-stream","messages":[{"role":"assistant","messageId":"a","content":"old","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","outputTokens":1}},{"role":"assistant","messageId":"b","content":"new"}]}"#,
    )
    .unwrap();
    assert!(
        attached
            .messages(Some(&path_text), Some(&session_id), &AmpAdapter)
            .is_empty(),
        "an in-flight assistant message is not completion-certified"
    );

    std::fs::write(
        &path,
        r#"{"id":"T-stream","messages":[{"role":"assistant","messageId":"a","content":"old","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","outputTokens":1}},{"role":"assistant","messageId":"b","content":"new","usage":{"timestamp":"2026-01-01T00:00:01Z","model":"gpt-5","outputTokens":1}}]}"#,
    )
    .unwrap();
    assert_eq!(
        attached.messages(Some(&path_text), Some(&session_id), &AmpAdapter),
        vec!["new"]
    );
    assert!(
        attached
            .messages(Some(&path_text), Some(&session_id), &AmpAdapter)
            .is_empty()
    );

    let mut from_start = crate::agents::transcript::TranscriptCursor::new(true);
    assert_eq!(
        from_start.messages(Some(&path_text), Some(&session_id), &AmpAdapter),
        vec!["old", "new"]
    );
}

#[test]
fn local_refresh_publishes_latest_tokens_estimated_cost_and_stat_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("T-live.json");
    let path_text = path.to_string_lossy().into_owned();
    std::fs::write(
        &path,
        r#"{"id":"T-live","messages":[{"role":"assistant","messageId":"a","content":"one","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","inputTokens":100,"outputTokens":10}},{"role":"assistant","messageId":"b","content":"two","usage":{"timestamp":"2026-01-01T00:00:01Z","model":"gpt-5","inputTokens":200,"outputTokens":20,"cacheCreationInputTokens":30,"cacheReadInputTokens":40}}]}"#,
    )
    .unwrap();
    let pricing = dir.path().join("pricing-cache.json");
    let ctx = LocalContextRefreshCtx {
        agent_id: "T-live",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: Some(&path_text),
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    };

    let refresh = AmpAdapter
        .local_context_refresh(RefreshTrigger::Hook("agent_end"), &ctx)
        .unwrap();
    assert_eq!(
        refresh.context.model_id.as_set().map(String::as_str),
        Some("gpt-5")
    );
    assert_eq!(
        refresh
            .context
            .tokens
            .as_value()
            .and_then(|tokens| tokens.current_usage.as_ref())
            .unwrap(),
        &AgentCurrentUsage {
            input_tokens: Some(200),
            output_tokens: Some(20),
            cache_creation_input_tokens: Some(30),
            cache_read_input_tokens: Some(40),
        }
    );
    assert!(
        refresh
            .context
            .cost
            .as_set()
            .and_then(|cost| cost.total_cost_usd)
            .is_some_and(|cost| cost > 0.0)
    );
    let stat = refresh.transcript_stat.unwrap();
    let gated = LocalContextRefreshCtx {
        prior_transcript_stat: Some(&stat),
        prior_spend_fold: None,
        ..ctx
    };
    assert!(
        AmpAdapter
            .local_context_refresh(RefreshTrigger::Tick, &gated)
            .is_none()
    );
    assert!(
        AmpAdapter
            .local_context_refresh(RefreshTrigger::Hook("tool_result"), &ctx)
            .is_none()
    );
}

#[test]
fn lifecycle_stamps_the_exact_cache_path_for_run_recording() {
    let dir = tempfile::tempdir().unwrap();
    let threads = dir.path().join("threads");
    std::fs::create_dir_all(&threads).unwrap();
    let path = threads.join("T-run.json");
    std::fs::write(&path, r#"{"id":"T-run","messages":[]}"#).unwrap();
    let mut observation = AmpAdapter
        .decode_hook("agent_end", &json!({"session_id":"T-run","status":"done"}))
        .expect("test hook decodes")
        .lifecycle
        .unwrap();

    stamp_transcript_path(&mut observation, "T-run", dir.path());
    assert_eq!(
        observation.transcript_path.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
}

#[test]
fn transcript_and_spend_join_into_timestamped_session_turns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("T-turns.json");
    std::fs::write(
        &path,
        r#"{"id":"T-turns","messages":[{"role":"user","messageId":"u1","timestamp":"2026-01-01T00:00:00Z","content":"fix it"},{"role":"assistant","messageId":"a1","timestamp":"2026-01-01T00:00:01Z","content":"done","usage":{"timestamp":"2026-01-01T00:00:05Z","model":"gpt-5","inputTokens":100,"outputTokens":20}}]}"#,
    )
    .unwrap();
    let messages = AmpAdapter.read_transcript_messages(&path, None).unwrap();
    let spend = AmpAdapter.parse_spend(&path, None, &PriceBook::embedded());

    let turns = crate::agents::turns::session_turns(&messages, &spend.entries, "T-turns", false);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].prompt, "fix it");
    assert_eq!(turns[0].fresh_input, 100);
    assert_eq!(turns[0].output, 20);
    assert_eq!(turns[0].ended_at.unwrap().as_second(), 1_767_225_605);
}
