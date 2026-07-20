use serde_json::json;

use super::*;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output, hook_signal};
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
        adapter.launch_command(&[], Some("")),
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
    let ask = hook_lifecycle(
        &adapter,
        "Notification",
        &json!({
            "sessionId":"s1",
            "notificationType":"permission_prompt",
            "message":"Plan approval requested"
        }),
    );
    assert!(matches!(
        ask.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::PlanApproval,
            ..
        }
    ));
    let interrupted = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"sessionId":"s1","reason":"cancelled"}),
    );
    assert_eq!(interrupted.signal, LifecycleSignal::TurnInterrupted);
    assert!(
        hook_observation(
            &adapter,
            "Stop",
            &json!({"sessionId":"s1","reason":"shutdown"})
        )
        .is_none()
    );
    let child = hook_lifecycle(
        &adapter,
        "SubagentStop",
        &json!({"sessionId":"s1","subagentId":"child-1","exitCode":-1}),
    );
    assert_eq!(child.agent_id.as_deref(), Some("child-1"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("s1"));
    assert_eq!(
        child.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    let failed_child = hook_lifecycle(
        &adapter,
        "SubagentStop",
        &json!({"sessionId":"s1","subagentId":"child-2","exitCode":1}),
    );
    assert_eq!(
        failed_child.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert!(hook_observation(&adapter, "SubagentStart", &json!({"sessionId":"s1"})).is_none());
    assert!(matches!(
        hook_signal(
            &adapter,
            "Stop",
            &json!({"sessionId":"s1","reason":"error"})
        ),
        LifecycleSignal::TurnEnded { errored: true, .. }
    ));
    let decoded = hook_output(&adapter, "permission_denied", &json!({}));
    assert_eq!(decoded.class(), AgentHookClass::Unknown);
    assert_eq!(decoded.ask_kind(), None);
    assert_eq!(decoded.event_name(), "PermissionDenied");
    assert_eq!(
        hook_output(&adapter, "future_event", &json!({})).event_name(),
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
        let observation = hook_lifecycle(
            &adapter,
            "PostToolUse",
            &json!({"sessionId":"s1","toolName":tool}),
        );
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
        hook_signal(
            &adapter,
            "PostToolUseFailure",
            &json!({"sessionId":"s1","toolName":"apply_patch","error":"denied"})
        ),
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        }
    );
    assert_eq!(
        hook_signal(
            &adapter,
            "PostCompact",
            &json!({"sessionId":"s1","source":"manual"})
        ),
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
        hook_output(&GrokAdapter, "Notification", &Value::Null)
            .json_reply()
            .cloned()
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
    assert_eq!(initial.context.settle, crate::agents::FieldPatch::Clear);

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
        requested
            .context
            .settle
            .as_set()
            .map(|settle| (settle.at, settle.outcome)),
        requested_at
            .parse()
            .ok()
            .map(|at| (at, crate::agents::TurnSettleOutcome::NativeWait))
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
    assert_eq!(resolved.context.settle, crate::agents::FieldPatch::Clear);
    assert!(
        hook_observation(
            &GrokAdapter,
            "permission_requested",
            &json!({"sessionId":"session-1","toolName":"run_terminal_command"})
        )
        .is_none()
    );
}

#[test]
fn completed_usage_replaces_mid_turn_scalar_and_estimates_missing_cost() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let updates = session.join("updates.jsonl");
    let pricing = dir.path().join("pricing-cache.json");
    std::fs::create_dir(&session).unwrap();
    let lines = [
        json!({
            "timestamp": 1_700_000_000_u64,
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": {"type": "text", "text": "ping"},
                    "_meta": {"promptIndex": 0}
                }
            }
        }),
        json!({
            "timestamp": 1_700_000_001_u64,
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": "thinking"}
                },
                "_meta": {"totalTokens": 9_171, "contextWindowTokens": 500_000}
            }
        }),
        json!({
            "timestamp": 1_700_000_002_u64,
            "method": "_x.ai/session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "turn_completed",
                    "prompt_id": "prompt-1",
                    "stop_reason": "end_turn",
                    "usage": {
                        "inputTokens": 17_869,
                        "cachedReadTokens": 0,
                        "outputTokens": 32,
                        "totalTokens": 17_901,
                        "modelUsage": {
                            "grok-4.5-build-free": {
                                "inputTokens": 17_869,
                                "cachedReadTokens": 0,
                                "outputTokens": 32,
                                "totalTokens": 17_901
                            }
                        }
                    }
                }
            }
        }),
    ]
    .map(|row| row.to_string())
    .join("\n");
    std::fs::write(&updates, format!("{lines}\n")).unwrap();
    let ctx = LocalContextRefreshCtx {
        agent_id: "session-1",
        model_hint: Some("grok-4.5"),
        current_transcript_path: None,
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    };

    let refresh = refresh_resolved_context(&updates, None, &ctx).unwrap();
    let tokens = refresh.context.tokens.clone().into_value().unwrap();
    assert_eq!(tokens.current_context_tokens, Some(17_869));
    assert_eq!(tokens.used_percentage, Some(4));
    let usage = tokens.current_usage.unwrap();
    assert_eq!(usage.input_tokens, Some(17_869));
    assert_eq!(usage.cache_read_input_tokens, Some(0));
    assert_eq!(usage.output_tokens, Some(32));
    assert!(
        refresh
            .context
            .cost
            .as_set()
            .and_then(|cost| cost.total_cost_usd)
            .is_some_and(|cost| cost > 0.0)
    );
}

#[test]
fn only_failure_hooks_contribute_turn_errors() {
    let adapter = GrokAdapter;
    assert!(
        hook_output(&adapter, "Stop", &json!({"reason":"end_turn"}))
            .turn_error()
            .cloned()
            .is_none()
    );
    assert!(
        hook_output(
            &adapter,
            "Notification",
            &json!({"notificationType":"permission_prompt"})
        )
        .turn_error()
        .cloned()
        .is_none()
    );
    assert_eq!(
        hook_output(
            &adapter,
            "Notification",
            &json!({"notificationType":"agent_error","message":"provider failed"})
        )
        .turn_error()
        .cloned()
        .and_then(|error| error.label),
        Some("provider failed".to_owned())
    );
}
