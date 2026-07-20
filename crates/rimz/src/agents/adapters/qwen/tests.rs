use std::fs;
use std::io::Write as _;

use serde_json::{Value, json};

use super::*;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output, hook_signal};
use crate::agents::transcript::TranscriptCursor;
use crate::agents::{AgentHookClass, AskKind, CostCoverage};

const REWOUND_SESSION: &str = include_str!("tests/fixtures/rewound-session.jsonl");

#[test]
fn classifies_native_asks_and_keeps_neutral_stdout_silent() {
    let adapter = QwenAdapter;
    for (tool_name, ask_kind) in [
        ("ask_user_question", AskKind::Question),
        ("exit_plan_mode", AskKind::PlanApproval),
        ("run_shell_command", AskKind::Permission),
    ] {
        let classified = hook_output(
            &adapter,
            "PermissionRequest",
            &json!({"tool_name":tool_name}),
        );
        assert_eq!(classified.class(), AgentHookClass::AwaitingUser);
        assert_eq!(classified.ask_kind(), Some(ask_kind));
    }
    let pre_tool = hook_output(
        &adapter,
        "PreToolUse",
        &json!({"tool_name":"ask_user_question"}),
    );
    assert_eq!(pre_tool.class(), AgentHookClass::AwaitingUser);
    assert_eq!(pre_tool.ask_kind(), Some(AskKind::Question));
    let ordinary = hook_output(
        &adapter,
        "PreToolUse",
        &json!({"tool_name":"run_shell_command"}),
    );
    assert_eq!(ordinary.class(), AgentHookClass::Lifecycle);
    assert_eq!(ordinary.ask_kind(), None);
    insta::assert_debug_snapshot!(hook_output(&adapter, "PermissionRequest", &Value::Null).json_reply().cloned(), @"None");
}

#[test]
fn launch_and_permission_argv_match_qwen_cli() {
    let adapter = QwenAdapter;
    assert_eq!(adapter.default_launch_model(), None);
    assert_eq!(
        adapter.launch_command(&[], None),
        Some(vec!["qwen".to_owned()])
    );
    assert_eq!(
        adapter.launch_command(
            &["--model".to_owned(), "qwen3".to_owned()],
            Some("start here")
        ),
        Some(vec![
            "qwen".to_owned(),
            "--model".to_owned(),
            "qwen3".to_owned(),
            "-i".to_owned(),
            "start here".to_owned(),
        ])
    );
    assert_eq!(
        adapter.launch_command(&[], Some("")),
        Some(vec!["qwen".to_owned()])
    );
    assert_eq!(
        adapter.spec().launch.permission_args(PermissionMode::Auto),
        ["--approval-mode", "auto-edit"]
    );
    assert_eq!(
        adapter.spec().launch.max_turns_args(7),
        Some(vec!["--max-session-turns".to_owned(), "7".to_owned()])
    );
    assert_eq!(adapter.spec().launch.compact_command(), Some("/compress"));
}

#[test]
fn installs_restores_and_leaves_preset_statusline_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, r#"{"ui":{"statusLine":{"type":"command","command":"myline","refreshInterval":5}},"theme":"dark"}"#).unwrap();
    let report = install::MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(report.installed_events.len(), 14);
    assert!(install::MANAGED_SOURCE.installed_at(&path));
    assert!(install::MANAGED_SOURCE.managed_artifacts_at(&path));
    assert!(!install::MANAGED_SOURCE.upgrade_available_at(&path));
    let installed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        installed
            .pointer("/ui/statusLine/command")
            .and_then(Value::as_str),
        Some(STATUS_LINE_COMMAND)
    );
    assert_eq!(
        installed
            .pointer("/ui/statusLine/_rimz_wrapped/command")
            .and_then(Value::as_str),
        Some("myline")
    );
    let pre_tool = installed
        .pointer("/hooks/PreToolUse/0")
        .and_then(Value::as_object)
        .expect("managed PreToolUse hook");
    assert_eq!(
        pre_tool.get("matcher").and_then(Value::as_str),
        Some("ask_user_question|exit_plan_mode")
    );
    assert_eq!(
        pre_tool
            .get("hooks")
            .and_then(Value::as_array)
            .and_then(|hooks| hooks.first())
            .and_then(|hook| hook.get("async"))
            .and_then(Value::as_bool),
        None
    );
    install::MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert!(!install::MANAGED_SOURCE.installed_at(&path));
    let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        restored
            .pointer("/ui/statusLine/command")
            .and_then(Value::as_str),
        Some("myline")
    );
    assert!(restored.get("hooks").is_none());

    fs::write(
        &path,
        r#"{"ui":{"statusLine":{"type":"preset","name":"minimal"}}}"#,
    )
    .unwrap();
    let preview = install::MANAGED_SOURCE.preview_at(&path).unwrap();
    assert_eq!(preview.status_line_change, None);
    let candidate: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    assert_eq!(
        candidate
            .pointer("/ui/statusLine/type")
            .and_then(Value::as_str),
        Some("preset")
    );

    for original in [
        json!({"ui": "compact"}),
        json!({"ui": {"statusLine": "compact"}}),
    ] {
        fs::write(&path, serde_json::to_string(&original).unwrap()).unwrap();
        let preview = install::MANAGED_SOURCE.preview_at(&path).unwrap();
        assert_eq!(preview.status_line_change, None);
        let candidate: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
        assert_eq!(candidate.get("ui"), original.get("ui"));
    }
}

#[test]
fn reinstall_reclaims_stale_and_matcherless_owned_pre_tool_use_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        format!(
            r#"{{"hooks":{{"PreToolUse":[{{"matcher":"exit_plan_mode|ask_user_question","_rimz_managed":true,"hooks":[{{"type":"command","command":"{RIMZ_HOOK_COMMAND}"}}]}},{{"_rimz_managed":true,"hooks":[{{"type":"command","command":"{RIMZ_HOOK_COMMAND}"}}]}},{{"matcher":"custom_tool","hooks":[{{"type":"command","command":"custom-hook"}}]}}]}}}}"#
        ),
    )
    .unwrap();

    install::MANAGED_SOURCE.install_into(&path).unwrap();

    let installed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let pre_tool = installed
        .pointer("/hooks/PreToolUse")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(pre_tool.len(), 2);
    let matchers = pre_tool
        .iter()
        .filter_map(|entry| entry.get("matcher").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        matchers,
        ["custom_tool", "ask_user_question|exit_plan_mode"]
    );
}

#[test]
fn rejects_async_managed_blocking_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, format!(r#"{{"hooks":{{"PermissionRequest":[{{"_rimz_managed":true,"hooks":[{{"type":"command","command":"{RIMZ_HOOK_COMMAND}","async":true}}]}}]}}}}"#)).unwrap();
    let error = install::MANAGED_SOURCE
        .install_into(&path)
        .unwrap_err()
        .to_string();
    assert!(error.contains("async"));

    fs::write(&path, format!(r#"{{"hooks":{{"PreToolUse":[{{"matcher":"ask_user_question|exit_plan_mode","_rimz_managed":true,"hooks":[{{"type":"command","command":"{RIMZ_HOOK_COMMAND}","async":true}}]}}]}}}}"#)).unwrap();
    let error = install::MANAGED_SOURCE
        .install_into(&path)
        .unwrap_err()
        .to_string();
    assert!(error.contains("PreToolUse"));
    assert!(error.contains("async"));
}

#[test]
fn maps_prompts_and_permission_requests_to_lifecycle_signals() {
    let adapter = QwenAdapter;
    for prompt in ["", "  "] {
        assert_eq!(
            hook_observation(
                &adapter,
                "UserPromptSubmit",
                &json!({"session_id":"s1","prompt":prompt})
            ),
            None
        );
    }
    for payload in [json!({"session_id":"s1","prompt":"fix the bug"}), json!({})] {
        assert_eq!(
            hook_signal(&adapter, "UserPromptSubmit", &payload),
            LifecycleSignal::TurnStarted
        );
    }
    for (tool_name, kind) in [
        ("ask_user_question", AskKind::Question),
        ("exit_plan_mode", AskKind::PlanApproval),
        ("run_shell_command", AskKind::Permission),
    ] {
        assert_eq!(
            hook_signal(
                &adapter,
                "PermissionRequest",
                &json!({"session_id":"s1","tool_name":tool_name})
            ),
            LifecycleSignal::AwaitingInput {
                kind,
                ask_id: None,
                detail: None,
                native_key: None,
            }
        );
    }
    assert_eq!(
        hook_signal(
            &adapter,
            "PermissionRequest",
            &json!({"session_id":"s1","tool_name":"ask_user_question","tool_use_id":"ask-1"})
        ),
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: None,
            native_key: Some("ask-1".to_owned()),
        }
    );
    for (tool_name, kind, native_key) in [
        ("ask_user_question", Some(AskKind::Question), Some("ask-1")),
        (
            "exit_plan_mode",
            Some(AskKind::PlanApproval),
            Some("plan-1"),
        ),
        ("run_shell_command", None, Some("shell-1")),
    ] {
        let observed = hook_observation(
            &adapter,
            "PreToolUse",
            &json!({"session_id":"s1","tool_name":tool_name,"tool_use_id":native_key}),
        );
        assert_eq!(
            observed.map(|observation| observation.signal),
            kind.map(|kind| LifecycleSignal::AwaitingInput {
                kind,
                ask_id: None,
                detail: None,
                native_key: native_key.map(ToOwned::to_owned),
            })
        );
    }
}

#[test]
fn reads_gate_details_from_permission_requests() {
    let adapter = QwenAdapter;
    let question = json!({
        "tool_name":"ask_user_question",
        "tool_input":{
            "questions":[{
                "question":"Which path?\nMore context",
                "options":[{"label":"Safe","description":"Keep the current behavior"}],
                "multiSelect":false
            }]
        }
    });
    let questions = hook_output(&adapter, "PermissionRequest", &question)
        .questions()
        .to_vec();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].question, "Which path?\nMore context");
    assert_eq!(
        hook_output(&adapter, "PreToolUse", &question)
            .questions()
            .to_vec()[0]
            .question,
        "Which path?\nMore context"
    );
    assert_eq!(
        hook_output(&adapter, "PermissionRequest", &question)
            .ask_detail()
            .map(str::to_owned)
            .as_deref(),
        Some("Which path?")
    );

    let permission = json!({
        "tool_name":"run_shell_command",
        "tool_input":{"command":"cargo xtask gate"}
    });
    assert_eq!(
        hook_output(&adapter, "PermissionRequest", &permission)
            .ask_detail()
            .map(str::to_owned)
            .as_deref(),
        Some(r#"run_shell_command: {"command":"cargo xtask gate"}"#)
    );
    assert_eq!(
        hook_output(&adapter, "PreToolUse", &question)
            .ask_detail()
            .map(str::to_owned)
            .as_deref(),
        Some("Which path?")
    );
}

#[test]
fn shared_questionnaire_and_permission_normalization_preserves_qwen_details() {
    let questions = hook_output(
        &QwenAdapter,
        "PermissionRequest",
        &json!({
            "tool_name": "ask_user_question",
            "tool_input": {
                "questions": [
                    {"question": "Camel?", "multiSelect": true},
                    {"question": "Snake?", "multi_select": true},
                    {"question": "OpenCode?", "multiple": true, "options": [
                        {"label": " keep ", "description": "   "},
                        {"label": "   "}
                    ]},
                    {"question": "   "}
                ]
            }
        }),
    )
    .questions()
    .to_vec();
    assert_eq!(questions.len(), 3);
    assert!(questions.iter().all(|question| question.multi_select));
    assert_eq!(questions[2].options.len(), 1);
    assert_eq!(questions[2].options[0].description, None);

    let plan = hook_output(
        &QwenAdapter,
        "PermissionRequest",
        &json!({"tool_name": "exit_plan_mode", "tool_input": {"plan": " Ship it "}}),
    )
    .questions()
    .to_vec();
    assert_eq!(plan[0].question, "Requesting plan approval:\n\nShip it");
    assert!(plan[0].options.is_empty());

    for payload in [
        json!({"tool_name": " run_shell_command "}),
        json!({"tool_name": " run_shell_command ", "tool_input": {}}),
        json!({"tool_name": " run_shell_command ", "tool_input": null}),
    ] {
        assert_eq!(
            hook_output(&QwenAdapter, "PermissionRequest", &payload).ask_detail(),
            Some("run_shell_command")
        );
    }

    let detail = hook_output(
        &QwenAdapter,
        "PermissionRequest",
        &json!({
            "tool_name": "run_shell_command",
            "tool_input": {"command": "x".repeat(200)}
        }),
    )
    .ask_detail()
    .expect("detail")
    .to_owned();
    assert_eq!(
        detail
            .strip_prefix("run_shell_command: ")
            .expect("summary")
            .chars()
            .count(),
        160
    );
}

#[test]
fn maps_lifecycle_context_background_and_subagents() {
    let adapter = QwenAdapter;
    let start = hook_lifecycle(
        &adapter,
        "SessionStart",
        &json!({"session_id":"s1","source":"startup","model":"qwen3"}),
    );
    assert_eq!(start.signal, LifecycleSignal::Registered);
    assert_eq!(start.origin, Some(SessionOrigin::Fresh));
    let tool = hook_lifecycle(
        &adapter,
        "PostToolUse",
        &json!({"session_id":"s1","tool_name":"write_file","tool_use_id":"write-1"}),
    );
    assert_eq!(
        tool.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
            native_key: Some("write-1".to_owned()),
        }
    );
    let parked = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"session_id":"s1","background_tasks":[{"status":"running"}],"context_usage":1.2,"context_limit":131072}),
    );
    assert_eq!(
        parked.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true
        }
    );
    assert_eq!(parked.usage.context_pct, Some(100));
    assert_eq!(parked.usage.context_window, Some(131072));
    let child = hook_lifecycle(
        &adapter,
        "SubagentStart",
        &json!({"session_id":"parent","agent_id":"child","agent_type":"review"}),
    );
    assert_eq!(child.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(child.task.as_deref(), Some("review"));
    assert!(child.parent_agent_id.is_some());
    assert_eq!(
        hook_signal(
            &adapter,
            "SessionStart",
            &json!({"session_id":"s1","source":"compact"})
        ),
        LifecycleSignal::CompactionEnded { auto: None }
    );
}

#[test]
fn transcript_tail_and_statusline_supply_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1.jsonl");
    fs::write(&path, "{\"uuid\":\"a1\",\"type\":\"assistant\",\"model\":\"qwen3\",\"contextWindowSize\":131072,\"usageMetadata\":{\"totalTokenCount\":420}}\n{\"uuid\":\"title-1\",\"parentUuid\":\"a1\",\"type\":\"system\",\"subtype\":\"custom_title\",\"systemPayload\":{\"customTitle\":\"Stable Qwen title\"}}\n").unwrap();
    let adapter = QwenAdapter;
    let observation = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"session_id":"s1","transcript_path":path}),
    );
    assert_eq!(observation.usage.total_tokens, Some(420));
    assert_eq!(observation.usage.context_window, Some(131072));
    assert_eq!(
        observation.description.as_deref(),
        Some("Stable Qwen title")
    );

    let context = adapter.observe_context("qwen", &json!({
        "session_id":"s1",
        "version":"0.19.10",
        "model":{"display_name":"[DeepSeek] deepseek-v4-pro"},
        "context_window":{"context_window_size":1000000,"used_percentage":3.9,"remaining_percentage":96.1,"current_usage":38727,"total_input_tokens":30000,"total_output_tokens":5000},
        "metrics":{"models":{"qwen3":{"tokens":{"prompt":30000,"completion":5000,"cached":10000,"thoughts":2000}}},"files":{"total_lines_added":12,"total_lines_removed":3}},
        "vim":{"mode":"INSERT"}
    })).unwrap().context;
    assert_eq!(
        context.model_display_name.as_deref(),
        Some("DeepSeek V4 Pro")
    );
    let malformed_context = adapter
        .observe_context(
            "qwen",
            &json!({"session_id":"s1","model":{"display_name":"[DeepSeek]"}}),
        )
        .unwrap()
        .context;
    assert_eq!(
        malformed_context.model_display_name.as_deref(),
        Some("[DeepSeek]")
    );
    assert_eq!(
        context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(4)
    );
    let tokens = context.tokens.as_ref().unwrap();
    assert_eq!(tokens.context_window_size, Some(1_000_000));
    assert_eq!(tokens.remaining_percentage, Some(96));
    assert_eq!(tokens.current_context_tokens, Some(38_727));
    assert_eq!(tokens.current_usage, None);
    assert_eq!(tokens.session_usage, None);
    assert_eq!(
        context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_lines_added),
        Some(12)
    );
}

#[test]
fn subagent_stop_reads_model_and_description_from_meta_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("projects/project-a");
    let transcript = project.join("chats/parent.jsonl");
    let meta = project.join("subagents/parent/agent-child.meta.json");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::create_dir_all(meta.parent().unwrap()).unwrap();
    fs::write(&transcript, "").unwrap();
    fs::write(
        &meta,
        r#"{
            "agentId":"child",
            "agentType":"general-purpose",
            "subagentName":"general-purpose",
            "description":"Inspect the lifecycle seam",
            "createdAt":"2026-07-16T12:50:49.268Z",
            "persistedCliFlags":{"model":"deepseek-v4-pro"}
        }"#,
    )
    .unwrap();

    let observation = hook_lifecycle(
        &QwenAdapter,
        "SubagentStop",
        &json!({
            "session_id":"parent",
            "agent_id":"child",
            "agent_type":"general-purpose",
            "transcript_path":transcript,
        }),
    );
    assert_eq!(observation.launch.model.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(
        observation.description.as_deref(),
        Some("Inspect the lifecycle seam")
    );
    assert_eq!(observation.task.as_deref(), Some("general-purpose"));
}

#[test]
fn statusline_uses_numeric_string_context_occupancy_without_session_categories() {
    let context = QwenAdapter
        .observe_context(
            "qwen",
            &json!({
                "session_id": "s1",
                "context_window": {
                    "context_window_size": 1_000_000,
                    "used_percentage": 7.2,
                    "current_usage": "72000"
                },
                "metrics": {
                    "models": {
                        "model-a": {
                            "tokens": {
                                "prompt": "100",
                                "completion": "50",
                                "cached": "25",
                                "thoughts": "20",
                                "future_counter": 99
                            }
                        },
                        "model-b": {
                            "tokens": {
                                "prompt": 200,
                                "completion": 30,
                                "cached": 50,
                                "thoughts": 40
                            }
                        },
                        "malformed-future-model": true
                    }
                }
            }),
        )
        .unwrap()
        .context;
    let tokens = context.tokens.unwrap();
    assert_eq!(tokens.context_window_size, Some(1_000_000));
    assert_eq!(tokens.used_percentage, Some(7));
    assert_eq!(tokens.current_context_tokens, Some(72_000));
    assert_eq!(tokens.current_usage, None);
    assert_eq!(tokens.session_usage, None);
}

#[test]
fn statusline_context_occupancy_preserves_zero_absence_and_malformed_values() {
    let absent = QwenAdapter
        .observe_context(
            "qwen",
            &json!({
                "session_id": "s1",
                "context_window": {"context_window_size": 10_000},
                "metrics": {
                    "models": {
                        "empty": {"tokens": {}},
                        "unaccounted-output": {"tokens": {"completion": "50"}}
                    }
                }
            }),
        )
        .unwrap()
        .context;
    let tokens = absent.tokens.unwrap();
    assert_eq!(tokens.current_context_tokens, None);
    assert_eq!(tokens.current_usage, None);
    assert_eq!(tokens.session_usage, None);

    let zero = QwenAdapter
        .observe_context(
            "qwen",
            &json!({
                "session_id": "s1",
                "context_window": {"current_usage": 0}
            }),
        )
        .unwrap()
        .context;
    assert_eq!(zero.tokens.unwrap().current_context_tokens, Some(0));

    let malformed = QwenAdapter
        .observe_context(
            "qwen",
            &json!({
                "session_id": "s1",
                "context_window": {
                    "context_window_size": 10_000,
                    "current_usage": "unknown"
                }
            }),
        )
        .unwrap()
        .context;
    let tokens = malformed.tokens.unwrap();
    assert_eq!(tokens.current_context_tokens, None);
    assert_eq!(tokens.session_usage, None);
}

#[test]
fn statusline_cost_prices_every_decorated_routed_model() {
    let prices = PriceBook::from_litellm_json(
        r#"{
            "model-a": {"input_cost_per_token": 0.000001, "output_cost_per_token": 0.000002, "cache_read_input_token_cost": 0.0000001},
            "model-b": {"input_cost_per_token": 0.00001, "output_cost_per_token": 0.00002, "cache_read_input_token_cost": 0.000001}
        }"#,
    );
    let payload = json!({
        "model": {"display_name": "model-a"},
        "metrics": {
            "models": {
                "[Provider A] model-a": {"tokens": {"prompt": 100, "total": 120, "cached": 40}},
                "model-b": {"tokens": {"prompt": 10, "total": 15, "cached": 2}}
            }
        }
    });
    let cost = QwenAdapter.context_cost(&payload, &prices).unwrap();
    assert_eq!(cost.coverage, CostCoverage::Session);
    assert!((cost.total_cost_usd.unwrap() - 0.000_286).abs() < 1e-15);
}

#[test]
fn statusline_cost_requires_every_material_bucket_to_be_priceable() {
    let prices = PriceBook::from_litellm_json(
        r#"{
            "model-a": {"input_cost_per_token": 0.000001, "output_cost_per_token": 0.000002}
        }"#,
    );
    let partial = json!({
        "metrics": {
            "models": {
                "model-a": {"tokens": {"prompt": 100, "total": 120}},
                "unknown": {"tokens": {"prompt": 1, "total": 2}}
            }
        }
    });
    assert_eq!(QwenAdapter.context_cost(&partial, &prices), None);

    let complete = json!({
        "metrics": {
            "models": {
                "model-a": {"tokens": {"prompt": 100, "total": 120}},
                "unknown-zero": {"tokens": {"prompt": 0, "total": 0}},
                "empty": {"tokens": {}},
                "malformed": true
            }
        }
    });
    let cost = QwenAdapter.context_cost(&complete, &prices).unwrap();
    assert_eq!(cost.coverage, CostCoverage::Session);
    assert!((cost.total_cost_usd.unwrap() - 0.000_14).abs() < 1e-15);
}

#[test]
fn rewound_transcript_supplies_active_hook_boundary_usage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rewound-session.jsonl");
    fs::write(&path, REWOUND_SESSION).unwrap();
    let adapter = QwenAdapter;
    for event in ["SessionStart", "Stop"] {
        let observation = hook_lifecycle(
            &adapter,
            event,
            &json!({
                "hook_event_name": event,
                "session_id": "sess-rewind",
                "source": "startup",
                "transcript_path": path,
                "input_tokens": 450
            }),
        );
        assert_eq!(
            observation.launch.model.as_deref(),
            Some("qwen-active-final")
        );
        assert_eq!(observation.usage.total_tokens, Some(555));
        assert_eq!(observation.usage.context_window, Some(333_333));
        assert_eq!(observation.usage.cache_read_input_tokens, Some(50));
        assert_eq!(observation.usage.fresh_input_tokens, Some(400));
        assert_eq!(observation.usage.output_tokens, Some(105));
    }
}

#[test]
fn successive_stops_publish_correlated_small_call_splits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("successive.jsonl");
    fs::write(
        &path,
        concat!(
            r#"{"uuid":"u1","type":"user"}"#,
            "\n",
            r#"{"uuid":"a1","parentUuid":"u1","type":"assistant","model":"deepseek-v4-pro","contextWindowSize":1000000,"usageMetadata":{"promptTokenCount":38727,"cachedContentTokenCount":38656,"candidatesTokenCount":85,"thoughtsTokenCount":77,"totalTokenCount":38812}}"#,
            "\n"
        ),
    )
    .unwrap();
    let adapter = QwenAdapter;
    let first = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"session_id":"s1","transcript_path":path,"input_tokens":38727}),
    );
    assert_eq!(first.usage.total_tokens, Some(38_812));
    assert_eq!(first.usage.cache_read_input_tokens, Some(38_656));
    assert_eq!(first.usage.fresh_input_tokens, Some(71));
    assert_eq!(first.usage.output_tokens, Some(85));

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, r#"{{"uuid":"u2","parentUuid":"a1","type":"user"}}"#).unwrap();
    writeln!(file, r#"{{"uuid":"a2","parentUuid":"u2","type":"assistant","model":"deepseek-v4-pro","contextWindowSize":1000000,"usageMetadata":{{"promptTokenCount":38735,"cachedContentTokenCount":38656,"candidatesTokenCount":92,"thoughtsTokenCount":80,"totalTokenCount":38827}}}}"#).unwrap();
    let second = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"session_id":"s1","transcript_path":path,"input_tokens":38735}),
    );
    assert_eq!(second.usage.total_tokens, Some(38_827));
    assert_eq!(second.usage.cache_read_input_tokens, Some(38_656));
    assert_eq!(second.usage.fresh_input_tokens, Some(79));
    assert_eq!(second.usage.output_tokens, Some(92));
}

#[test]
fn all_cached_and_stale_transcripts_keep_categories_honest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        r#"{"uuid":"a1","type":"assistant","usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":100,"candidatesTokenCount":5,"totalTokenCount":105}}"#,
    )
    .unwrap();
    let adapter = QwenAdapter;
    let cached = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"session_id":"s1","transcript_path":path,"input_tokens":100}),
    );
    assert_eq!(cached.usage.cache_read_input_tokens, Some(100));
    assert_eq!(cached.usage.fresh_input_tokens, Some(0));
    assert_eq!(cached.usage.output_tokens, Some(5));

    fs::write(&path, "").unwrap();
    let fresh = hook_lifecycle(
        &adapter,
        "SessionStart",
        &json!({"session_id":"s2","source":"startup","transcript_path":path}),
    );
    assert_eq!(fresh.usage.total_tokens, Some(0));
    assert_eq!(fresh.usage.cache_read_input_tokens, None);
    assert_eq!(fresh.usage.fresh_input_tokens, None);
    assert_eq!(fresh.usage.output_tokens, None);

    fs::write(
        &path,
        r#"{"uuid":"stale","type":"assistant","model":"stale-model","contextWindowSize":123456,"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":90,"candidatesTokenCount":5,"totalTokenCount":105}}"#,
    )
    .unwrap();
    let stopped = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"session_id":"s2","transcript_path":path,"input_tokens":200}),
    );
    assert_eq!(stopped.usage.total_tokens, Some(200));
    assert_eq!(stopped.launch.model, None);
    assert_eq!(stopped.usage.context_window, None);
    assert_eq!(stopped.usage.cache_read_input_tokens, None);
    assert_eq!(stopped.usage.fresh_input_tokens, None);
    assert_eq!(stopped.usage.output_tokens, None);

    let explicit = hook_lifecycle(
        &adapter,
        "Stop",
        &json!({"session_id":"s2","transcript_path":path,"input_tokens":200,"total_tokens":999}),
    );
    assert_eq!(explicit.usage.total_tokens, Some(999));
}

#[test]
fn rewound_fixture_replays_only_the_active_root_branch() {
    let messages = QwenAdapter.parse_transcript_messages(REWOUND_SESSION);
    let text = messages
        .iter()
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        text,
        [
            "Start the task",
            "Initial answer",
            "Replacement question",
            "Replacement answer"
        ]
    );
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert!(messages.iter().all(|message| message.at.is_some()));
}

#[test]
fn transcript_cursor_streams_physical_appends_across_a_rewind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stream.jsonl");
    fs::write(
        &path,
        concat!(
            r#"{"uuid":"u1","type":"user","message":{"parts":[{"text":"old user"}]}}"#,
            "\n",
            r#"{"uuid":"a1","parentUuid":"u1","type":"assistant","message":{"parts":[{"text":"old answer"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let path_text = path.to_string_lossy().into_owned();
    let mut cursor = TranscriptCursor::new(false);
    assert!(
        cursor
            .messages(
                Some(&path_text),
                None,
                crate::agents::definition_by_kind("qwen").unwrap()
            )
            .is_empty()
    );

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        r#"{{"uuid":"rewind","parentUuid":"a1","type":"system"}}"#
    )
    .unwrap();
    writeln!(file, r#"{{"uuid":"u2","parentUuid":"rewind","type":"user","message":{{"parts":[{{"text":"replacement user"}}]}}}}"#).unwrap();
    writeln!(file, r#"{{"uuid":"a2","parentUuid":"u2","type":"assistant","message":{{"parts":[{{"text":"replacement answer"}}]}}}}"#).unwrap();
    assert_eq!(
        cursor.messages(
            Some(&path_text),
            None,
            crate::agents::definition_by_kind("qwen").unwrap()
        ),
        ["replacement answer"]
    );
    assert!(
        cursor
            .messages(
                Some(&path_text),
                None,
                crate::agents::definition_by_kind("qwen").unwrap()
            )
            .is_empty()
    );

    let mut from_start = TranscriptCursor::new(true);
    assert_eq!(
        from_start.messages(
            Some(&path_text),
            None,
            crate::agents::definition_by_kind("qwen").unwrap()
        ),
        ["old answer", "replacement answer"]
    );
}

#[test]
fn parses_legacy_main_thread_transcript_and_excludes_sidechains() {
    let adapter = QwenAdapter;
    let lines = concat!(
        r#"{"type":"user","timestamp":"2026-06-02T10:00:00Z","message":{"role":"user","parts":[{"text":"hello"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"model","parts":[{"text":"thinking...","thought":true},{"text":"hi there"}]}}"#,
        "\n",
        r#"{"type":"assistant","agentId":"child","message":{"role":"model","parts":[{"text":"child work"}]}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"message":{"role":"model","parts":[{"text":"sidechain"}]}}"#,
        "\n",
        r#"{"type":"tool_result","message":{"role":"user","parts":[{"functionResponse":{}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"model","parts":[{"functionCall":{"name":"edit"}}]}}"#,
    );
    let messages = adapter.parse_transcript_messages(lines);
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].role,
        crate::agents::transcript::TranscriptRole::User
    );
    assert_eq!(messages[0].text, "hello");
    assert!(messages[0].at.is_some());
    assert_eq!(
        messages[1].role,
        crate::agents::transcript::TranscriptRole::Assistant
    );
    // Thought parts and tool-call-only records leave no visible prose.
    assert_eq!(messages[1].text, "hi there");
    // Streaming filters to assistant text for `--wait`/`-p --stream`.
    assert_eq!(adapter.stream_assistant_messages(lines), vec!["hi there"]);
}

#[test]
fn stop_failure_maps_retryable_classes() {
    let adapter = QwenAdapter;
    assert_eq!(
        hook_output(&adapter, "StopFailure", &json!({"error":"rate_limit"}))
            .turn_error()
            .cloned()
            .unwrap()
            .class,
        TurnErrorClass::PausedRateLimit
    );
    assert_eq!(
        hook_output(&adapter, "StopFailure", &json!({"error":"server_error"}))
            .turn_error()
            .cloned()
            .unwrap()
            .class,
        TurnErrorClass::PausedOverloaded
    );
}
