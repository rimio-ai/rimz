use serde_json::json;

use super::*;
use crate::agents::{AgentHookClass, ClassificationSample, ClassifiedHook};

pub(super) fn classification_corpus() -> Vec<ClassificationSample> {
    let mut samples = crate::agents::hook_types::catalog_classification_corpus(GROK_HOOKS);
    samples.extend([
        sample(
            "notification",
            json!({"notificationType":"permission_prompt","message":"Plan approval requested"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::PlanApproval),
            "Notification",
        ),
        sample(
            "notification",
            json!({"notificationType":"permission_prompt","message":"Diff review requested"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::Permission),
            "Notification",
        ),
        sample(
            "notification",
            json!({"notificationType":"elicitation_dialog","message":"User question requested"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::Question),
            "Notification",
        ),
        sample(
            "notification",
            json!({"notificationType":"permission_prompt","message":"Tool Permission Requested"}),
            AgentHookClass::Lifecycle,
            None,
            "Notification",
        ),
        sample(
            "postToolUse",
            json!({"toolName":"run_terminal_command"}),
            AgentHookClass::Lifecycle,
            None,
            "PostToolUse",
        ),
        sample(
            "session_start",
            json!({"sessionId":"s1"}),
            AgentHookClass::Lifecycle,
            None,
            "SessionStart",
        ),
    ]);
    samples
}

fn sample(
    event_name: &'static str,
    payload: Value,
    class: AgentHookClass,
    ask_kind: Option<AskKind>,
    canonical: &str,
) -> ClassificationSample {
    ClassificationSample {
        event_name,
        payload,
        expected: ClassifiedHook {
            class,
            ask_kind,
            event_name: canonical.to_owned(),
        },
    }
}

#[test]
fn launch_keeps_streaming_flags_out_of_interactive_sessions() {
    let adapter = GrokAdapter;
    assert_eq!(
        adapter.launch_command(&[], None),
        Some(vec!["grok".to_owned()])
    );
    assert_eq!(
        adapter.launch_command(&["--max-turns".to_owned(), "3".to_owned()], Some("ping")),
        Some(vec![
            "grok".to_owned(),
            "--max-turns".to_owned(),
            "3".to_owned(),
            "-p".to_owned(),
            "ping".to_owned(),
            "--output-format".to_owned(),
            "streaming-json".to_owned(),
        ])
    );
    assert_eq!(
        adapter.resume_command("session-1", Path::new("/tmp")),
        Some(vec![
            "grok".to_owned(),
            "--resume".to_owned(),
            "session-1".to_owned(),
        ])
    );
    assert_eq!(
        resumed_session_id("/usr/local/bin/grok --resume=session-1").as_deref(),
        Some("session-1")
    );
}

#[test]
fn lifecycle_maps_exact_asks_stop_reasons_and_subagent_cancellation() {
    let adapter = GrokAdapter;
    let ask = adapter
        .decode_hook(
            "Notification",
            &json!({
                "sessionId":"s1",
                "notificationType":"permission_prompt",
                "message":"Plan approval requested"
            }),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert!(matches!(
        ask.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::PlanApproval,
            ..
        }
    ));
    let interrupted = adapter
        .decode_hook("Stop", &json!({"sessionId":"s1","reason":"cancelled"}))
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(interrupted.signal, LifecycleSignal::TurnInterrupted);
    assert!(
        adapter
            .decode_hook("Stop", &json!({"sessionId":"s1","reason":"shutdown"}))
            .expect("test hook decodes")
            .lifecycle()
            .is_none()
    );
    let child = adapter
        .decode_hook(
            "SubagentStop",
            &json!({"sessionId":"s1","subagentId":"child-1","exitCode":-1}),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(child.agent_id.as_deref(), Some("child-1"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("s1"));
    assert_eq!(
        child.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    let failed_child = adapter
        .decode_hook(
            "SubagentStop",
            &json!({"sessionId":"s1","subagentId":"child-2","exitCode":1}),
        )
        .expect("test hook decodes")
        .lifecycle()
        .unwrap();
    assert_eq!(
        failed_child.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert!(
        adapter
            .decode_hook("SubagentStart", &json!({"sessionId":"s1"}))
            .expect("test hook decodes")
            .lifecycle()
            .is_none()
    );
    assert!(matches!(
        adapter
            .decode_hook("Stop", &json!({"sessionId":"s1","reason":"error"}))
            .expect("test hook decodes")
            .lifecycle()
            .unwrap()
            .signal,
        LifecycleSignal::TurnEnded { errored: true, .. }
    ));
    let decoded = adapter
        .decode_hook("permission_denied", &json!({}))
        .expect("test hook decodes");
    assert_eq!(decoded.class(), AgentHookClass::Unknown);
    assert_eq!(decoded.ask_kind(), None);
    assert_eq!(decoded.event_name(), "PermissionDenied");
    assert_eq!(
        adapter
            .decode_hook("future_event", &json!({}))
            .expect("test hook decodes")
            .event_name(),
        "FutureEvent"
    );
}

#[test]
fn lifecycle_classifies_tool_effects_and_compaction_source() {
    let adapter = GrokAdapter;
    for (tool, mutates, edits) in [
        ("apply_patch", true, true),
        ("run_terminal_command", true, false),
        ("read", false, false),
    ] {
        let observation = adapter
            .decode_hook("PostToolUse", &json!({"sessionId":"s1","toolName":tool}))
            .expect("test hook decodes")
            .lifecycle()
            .unwrap();
        assert_eq!(
            observation.signal,
            LifecycleSignal::ToolUsed {
                mutates,
                edits,
                native_key: None,
            }
        );
    }
    assert_eq!(
        adapter
            .decode_hook(
                "PostToolUseFailure",
                &json!({"sessionId":"s1","toolName":"apply_patch","error":"denied"}),
            )
            .expect("test hook decodes")
            .lifecycle()
            .unwrap()
            .signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        }
    );
    assert_eq!(
        adapter
            .decode_hook("PostCompact", &json!({"sessionId":"s1","source":"manual"}))
            .expect("test hook decodes")
            .lifecycle()
            .unwrap()
            .signal,
        LifecycleSignal::CompactionEnded { auto: Some(false) }
    );
}

#[test]
fn managed_catalog_is_passive_and_excludes_pre_tool_use() {
    assert_eq!(install::catalog().len(), 12);
    assert!(install::catalog().iter().all(|hook| !hook.synchronous));
    assert!(
        !install::catalog()
            .iter()
            .any(|hook| hook.event == "PreToolUse")
    );
    assert!(
        GrokAdapter
            .decode_hook("Notification", &Value::Null)
            .expect("test hook decodes")
            .neutral()
            .is_none()
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rimz.json");
    let preview = install::MANAGED_SOURCE.preview_at(&path).unwrap();
    let candidate: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    assert_eq!(RIMZ_HOOK_COMMAND, "rimz hooks feed --source grok");
    for hook in install::catalog() {
        let entries = candidate["hooks"][hook.event].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let handler = &entries[0]["hooks"][0];
        assert_eq!(handler["command"], RIMZ_HOOK_COMMAND);
        assert!(!handler["command"].as_str().unwrap().contains('$'));
    }
}

#[test]
fn local_context_refresh_tracks_events_only_permission_changes() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let updates = session.join("updates.jsonl");
    let events = session.join("events.jsonl");
    let pricing = dir.path().join("pricing-cache.json");
    std::fs::create_dir(&session).unwrap();
    std::fs::write(&updates, "{}\n").unwrap();
    std::fs::write(&events, "").unwrap();
    let ctx = LocalContextRefreshCtx {
        agent_id: "session-1",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    };
    let initial = refresh_resolved_context(&updates, Some(&events), &ctx).unwrap();
    assert_eq!(
        initial.context.native_permission_wait,
        crate::agents::FieldPatch::Clear
    );

    let requested_at = "2026-07-18T04:21:46.248Z";
    std::fs::write(
        &events,
        json!({
            "ts": requested_at,
            "type": "permission_requested",
            "tool_name": "run_terminal_command",
        })
        .to_string(),
    )
    .unwrap();
    let requested_ctx = LocalContextRefreshCtx {
        prior_transcript_stat: initial.transcript_stat.as_ref(),
        prior_spend_fold: None,
        ..ctx
    };
    let requested = refresh_resolved_context(&updates, Some(&events), &requested_ctx).unwrap();
    assert_eq!(
        requested.context.native_permission_wait.as_set().copied(),
        requested_at.parse().ok()
    );

    let unchanged_ctx = LocalContextRefreshCtx {
        prior_transcript_stat: requested.transcript_stat.as_ref(),
        prior_spend_fold: None,
        ..ctx
    };
    assert!(refresh_resolved_context(&updates, Some(&events), &unchanged_ctx).is_none());

    let resolved_at = "2026-07-18T04:22:00Z";
    std::fs::write(
        &events,
        format!(
            "{}\n{}",
            json!({
                "ts": requested_at,
                "type": "permission_requested",
                "tool_name": "run_terminal_command",
            }),
            json!({
                "ts": resolved_at,
                "type": "permission_resolved",
                "tool_name": "run_terminal_command",
            })
        ),
    )
    .unwrap();
    let resolved = refresh_resolved_context(&updates, Some(&events), &unchanged_ctx).unwrap();
    assert_eq!(
        resolved.context.native_permission_wait,
        crate::agents::FieldPatch::Clear
    );
    assert!(
        GrokAdapter
            .decode_hook(
                "permission_requested",
                &json!({"sessionId":"session-1","toolName":"run_terminal_command"}),
            )
            .expect("test hook decodes")
            .lifecycle()
            .is_none()
    );
}

#[test]
fn only_failure_hooks_contribute_turn_errors() {
    let adapter = GrokAdapter;
    assert!(
        adapter
            .decode_hook("Stop", &json!({"reason":"end_turn"}))
            .expect("test hook decodes")
            .turn_error()
            .is_none()
    );
    assert!(
        adapter
            .decode_hook(
                "Notification",
                &json!({"notificationType":"permission_prompt"}),
            )
            .expect("test hook decodes")
            .turn_error()
            .is_none()
    );
    assert_eq!(
        adapter
            .decode_hook(
                "Notification",
                &json!({"notificationType":"agent_error","message":"provider failed"}),
            )
            .expect("test hook decodes")
            .turn_error()
            .and_then(|error| error.label),
        Some("provider failed".to_owned())
    );
}
