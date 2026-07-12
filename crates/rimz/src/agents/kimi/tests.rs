use std::path::Path;

use serde_json::json;

use super::*;
use crate::agents::AgentHookClass;

#[test]
fn native_questions_and_permissions_use_distinct_hooks() {
    let question = json!({
        "session_id": "s1",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions":[{"question":"Ship it?","options":[],"multi_select":false}]}
    });
    let classified = KimiAdapter.classify_hook("PreToolUse", &question);
    assert_eq!(classified.class, AgentHookClass::AwaitingUser);
    assert_eq!(classified.ask_kind, Some(super::super::AskKind::Question));

    let plan_pre_tool = json!({"session_id":"s1","tool_name":"ExitPlanMode"});
    assert_eq!(
        KimiAdapter
            .classify_hook("PreToolUse", &plan_pre_tool)
            .class,
        AgentHookClass::Lifecycle
    );
    assert!(
        KimiAdapter
            .observe_lifecycle("PreToolUse", &plan_pre_tool)
            .is_none()
    );

    let plan_permission = json!({
        "session_id":"s1",
        "tool_call_id":"t1",
        "tool_name":"ExitPlanMode",
        "action":"Exit plan mode"
    });
    assert!(matches!(
        KimiAdapter
            .observe_lifecycle("PermissionRequest", &plan_permission)
            .unwrap()
            .signal,
        LifecycleSignal::AwaitingInput {
            kind: super::super::AskKind::PlanApproval,
            ..
        }
    ));

    let permission = json!({"session_id":"s1","tool_name":"Bash","action":"Run tests"});
    assert!(matches!(
        KimiAdapter
            .observe_lifecycle("PermissionRequest", &permission)
            .unwrap()
            .signal,
        LifecycleSignal::AwaitingInput {
            kind: super::super::AskKind::Permission,
            ..
        }
    ));
    insta::assert_snapshot!(format!("{:?}", KimiAdapter.render_neutral("PermissionRequest").unwrap()), @"None");
}

#[test]
fn permission_result_and_interrupt_clear_waiting_state() {
    let result = KimiAdapter
        .observe_lifecycle(
            "PermissionResult",
            &json!({"session_id":"s1","tool_name":"Bash","decision":"approved"}),
        )
        .unwrap();
    assert_eq!(
        result.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        }
    );
    assert!(matches!(
        KimiAdapter
            .observe_lifecycle(
                "Interrupt",
                &json!({"session_id":"s1","turn_id":"t1","reason":"cancelled"})
            )
            .unwrap()
            .signal,
        LifecycleSignal::TurnEnded { errored: false, .. }
    ));
}

#[test]
fn failed_tools_clear_waits_and_background_questions_do_not_open_them() {
    let failed = KimiAdapter
        .observe_lifecycle(
            "PostToolUseFailure",
            &json!({"session_id":"s1","tool_name":"AskUserQuestion"}),
        )
        .unwrap();
    assert_eq!(
        failed.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        }
    );

    let background = json!({
        "session_id":"s1",
        "tool_name":"AskUserQuestion",
        "tool_input":{"background":true,"questions":[{"question":"Ship it?"}]}
    });
    assert_eq!(
        KimiAdapter.classify_hook("PreToolUse", &background).class,
        AgentHookClass::Lifecycle
    );
    assert!(
        KimiAdapter
            .observe_lifecycle("PreToolUse", &background)
            .is_none()
    );
}

#[test]
fn prompt_parts_flags_tools_and_resume_match_kimi_code() {
    let prompt = json!({
        "session_id":"s1",
        "cwd":"/tmp/project",
        "prompt":[{"type":"text","text":"fix"},{"type":"image","url":"x"},{"type":"text","text":"parser"}]
    });
    let observed = KimiAdapter
        .observe_lifecycle("UserPromptSubmit", &prompt)
        .unwrap();
    assert_eq!(observed.prompt.as_deref(), Some("fix\nparser"));
    assert_eq!(
        KimiAdapter.permission_args(PermissionMode::Auto),
        ["--auto"]
    );
    assert_eq!(
        KimiAdapter.permission_args(PermissionMode::Yolo),
        ["--yolo"]
    );
    assert_eq!(
        KimiAdapter.resume_command("s1", Path::new("/tmp")).unwrap(),
        ["kimi", "--session", "s1"]
    );
    assert_eq!(
        KimiAdapter
            .launch_command(&["--yolo".to_owned()], Some("review"))
            .unwrap(),
        [
            "kimi",
            "--prompt",
            "review",
            "--output-format",
            "stream-json"
        ]
    );

    let write = KimiAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({"session_id":"s1","tool_name":"Write"}),
        )
        .unwrap();
    assert_eq!(
        write.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
        }
    );
    assert_eq!(
        KimiAdapter.descriptor().process_names,
        ["kimi", "kimi-code"]
    );
    assert_eq!(KimiAdapter.descriptor().extra_bin_dirs, [".kimi-code/bin"]);
}

#[test]
fn install_merge_includes_native_permission_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "default_model = \"custom\"\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"my-hook\"\n",
    )
    .unwrap();
    install::install(&path).unwrap();
    assert!(install::installed(&path));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("PermissionRequest"));
    assert!(text.contains("PermissionResult"));
    assert!(text.contains("Interrupt"));
    assert!(text.contains("my-hook"));
}

#[test]
fn session_index_resolves_valid_main_wire_and_rejects_escape() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("sessions/wd_project/s1");
    std::fs::create_dir_all(session.join("agents/main")).unwrap();
    std::fs::write(
        session.join("state.json"),
        r#"{"workDir":"/tmp/project","agents":{}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("session_index.jsonl"),
        format!(
            "{{\"sessionId\":\"s1\",\"sessionDir\":{},\"workDir\":\"/tmp/project\"}}\n{{\"sessionId\":\"s1\",\"sessionDir\":\"/tmp\",\"workDir\":\"/tmp/project\"}}\n",
            serde_json::to_string(&session).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(
        wire::session_dir_under(dir.path(), "s1", Some(Path::new("/tmp/project"))).as_deref(),
        Some(std::fs::canonicalize(&session).unwrap().as_path())
    );
    assert_eq!(
        wire::session_dir_under(dir.path(), "s1", Some(Path::new("/other"))),
        None
    );
}

#[test]
fn usage_records_drive_context_spend_and_additive_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions/wd/s1/agents/main/wire.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"metadata\",\"protocol_version\":\"1.4\"}\n",
            "{\"type\":\"usage.record\",\"time\":1770000000000,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"session\",\"usage\":{\"inputOther\":999}}\n",
            "{\"type\":\"usage.record\",\"time\":1770000001000,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"turn\",\"usage\":{\"inputOther\":40000,\"output\":20,\"inputCacheRead\":10000,\"inputCacheCreation\":0}}\n"
        ),
    )
    .unwrap();
    let records = wire::records_from_bytes(&std::fs::read(&path).unwrap());
    assert_eq!(wire::usage_records(&records).len(), 2);
    let stat = transcript_stat(&path).unwrap();
    let tokens = refresh_wire_path(&path, stat, None)
        .unwrap()
        .tokens
        .unwrap();
    assert_eq!(tokens.used_percentage, Some(19));
    assert_eq!(
        tokens.current_usage.unwrap().cache_read_input_tokens,
        Some(10_000)
    );

    let parsed = spend::parse(&path, None, &super::super::PriceBook::embedded());
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(
        parsed.entries[1].model.as_deref(),
        Some("moonshot/kimi-k2.5")
    );
    assert_eq!(parsed.entries[1].thread_id.as_deref(), Some("s1"));
    assert!(parsed.entries.iter().all(|entry| entry.cost_usd > 0.0));
}

#[test]
fn compaction_and_effective_model_config_drive_context() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        concat!(
            "default_model = \"small\"\n",
            "[models.small]\nmax_context_size = 64000\n",
            "[models.large]\nmax_context_size = 128000\n",
            "[models.large.overrides]\nmax_context_size = 96000\n",
        ),
    )
    .unwrap();
    assert_eq!(
        configured_context_window_at(&config, Some("large")),
        Some(96_000)
    );

    let records = wire::records_from_bytes(
        concat!(
            "{\"type\":\"config.update\",\"time\":1,\"modelAlias\":\"large\",\"thinkingEffort\":\"high\"}\n",
            "{\"type\":\"usage.record\",\"time\":2,\"model\":\"large\",\"usageScope\":\"session\",\"usage\":{\"inputOther\":90000}}\n",
            "{\"type\":\"context.apply_compaction\",\"time\":3,\"tokensBefore\":90000,\"tokensAfter\":12000}\n",
        )
        .as_bytes(),
    );
    assert_eq!(latest_model(&records).as_deref(), Some("large"));
    assert_eq!(latest_effort(&records).as_deref(), Some("high"));
    assert_eq!(wire::latest_context_tokens(&records), Some(12_000));
}

#[test]
fn wire_without_usage_emits_fresh_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wire.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"context.append_loop_event\",\"time\":1,\"event\":{\"type\":\"tool.result\"}}\n",
    )
    .unwrap();
    let stat = transcript_stat(&path).unwrap();
    let tokens = refresh_wire_path(&path, stat, None)
        .unwrap()
        .tokens
        .unwrap();
    assert_eq!(tokens.used_percentage, None);
    assert!(tokens.current_usage.unwrap().is_zero());
}

#[test]
fn flat_context_messages_hide_thinking_parts() {
    let lines = concat!(
        "{\"type\":\"context.append_message\",\"time\":1770000000000,\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        "{\"type\":\"context.append_message\",\"time\":1770000001000,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"think\",\"think\":\"secret\"},{\"type\":\"text\",\"text\":\"done\"}]}}\n"
    );
    let messages = KimiAdapter.parse_transcript_messages(lines);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, super::super::TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "done");
}

#[test]
fn quota_parser_accepts_nested_remaining_and_reset_spellings() {
    let snapshot = oauth_usage::parse_response(
        r#"{"limits":[{"detail":{"limit":100,"remaining":25,"resetAt":"2030-01-01T00:00:00Z"},"window":{"duration":5,"timeUnit":"HOUR"}}],"boosterWallet":{"balance":{"type":"BOOSTER","amount":500000000,"amountLeft":125000000},"monthlyChargeLimitEnabled":true,"monthlyChargeLimit":{"priceInCents":500,"currency":"USD"},"monthlyUsed":{"priceInCents":125,"currency":"USD"}}}"#,
    )
    .unwrap();
    let window = &snapshot.rate_limits.as_ref().unwrap().windows[0];
    assert_eq!(window.used_percentage, Some(75));
    assert_eq!(window.duration_mins, Some(300));
    assert_eq!(
        snapshot.extra_credits,
        Some(super::super::ExtraCredits::known(
            Some(1.25),
            Some(1.25),
            Some(5.0)
        ))
    );
}
