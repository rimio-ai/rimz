use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::agents::lifecycle::{LifecycleState, TurnPhase, step};
use crate::agents::transcript::TranscriptCursor;
use crate::agents::{
    AgentHookClass, AgentStatus, LaunchPreset, PresetErr, TranscriptPosition, TranscriptRole,
};

const TRANSCRIPT_FIXTURE: &str = include_str!("tests/fixtures/droid-0.170.0-transcript-v2.jsonl");
const SETTINGS_FIXTURE: &str =
    include_str!("tests/fixtures/droid-0.170.0-transcript-v2.settings.json");

fn transcript_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, TRANSCRIPT_FIXTURE).unwrap();
    std::fs::write(dir.path().join("session.settings.json"), SETTINGS_FIXTURE).unwrap();
    (dir, path)
}

#[test]
fn install_preview_reclaim_drift_and_uninstall_preserve_user_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{
          "model": "custom",
          "hooks": {
            "Notification": [
              { "hooks": [{ "type": "command", "command": "echo user" }] },
              { "hooks": [{ "type": "command", "command": "rimz hooks feed --source droid --event Notification" }] }
            ]
          }
        }"#,
    )
    .unwrap();

    let before = std::fs::read_to_string(&path).unwrap();
    let preview = MANAGED_SOURCE.preview_at(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    let report = MANAGED_SOURCE.install_into(&path).unwrap();
    assert!(report.files[0].existed);
    assert_eq!(
        preview.files[0].candidate,
        std::fs::read_to_string(&path).unwrap()
    );
    assert!(MANAGED_SOURCE.installed_at(&path));

    let mut root: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    let notification = root["hooks"]["Notification"].as_array().unwrap();
    assert_eq!(notification.len(), 2, "one user hook plus one managed hook");
    assert_eq!(root["model"], "custom");
    root["hooks"].as_object_mut().unwrap().remove("Stop");
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).unwrap();
    assert!(!MANAGED_SOURCE.installed_at(&path));
    assert!(MANAGED_SOURCE.managed_artifacts_at(&path));
    assert!(!MANAGED_SOURCE.upgrade_available_at(&path));
    MANAGED_SOURCE.install_into(&path).unwrap();
    assert!(MANAGED_SOURCE.installed_at(&path));

    let mut root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    root["hooks"]["PostToolUse"][0]["hooks"][0]["timeout"] = json!(60);
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).unwrap();
    assert!(
        !MANAGED_SOURCE.installed_at(&path),
        "timeout drift must re-offer the canonical hook merge"
    );
    MANAGED_SOURCE.install_into(&path).unwrap();
    assert!(MANAGED_SOURCE.installed_at(&path));

    let uninstall = MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert!(uninstall.files[0].existed);
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(root["model"], "custom");
    assert_eq!(root["hooks"]["Notification"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["hooks"]["Notification"][0]["hooks"][0]["command"],
        "echo user"
    );
}

#[test]
fn lifecycle_maps_basic_turn_tools_compaction_and_end() {
    let startup = DroidAdapter
        .decode_hook(
            "SessionStart",
            &json!({
                "session_id": "sess-1",
                "transcript_path": "/tmp/droid.jsonl",
                "cwd": "/tmp/project",
                "source": "startup"
            }),
        )
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    assert_eq!(startup.signal, LifecycleSignal::Registered);
    assert_eq!(startup.origin, Some(SessionOrigin::Fresh));
    assert_eq!(startup.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(startup.transcript_path.as_deref(), Some("/tmp/droid.jsonl"));

    let prompt = DroidAdapter
        .decode_hook(
            "UserPromptSubmit",
            &json!({"session_id": "sess-1", "prompt": "  fix auth  "}),
        )
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    let running = step(None, None, &prompt.signal).next;
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(running.phase, TurnPhase::Reasoning);

    for (tool, mutates, edits) in [
        ("Edit", true, true),
        ("Execute", true, false),
        ("Read", false, false),
    ] {
        assert_eq!(
            DroidAdapter
                .decode_hook(
                    "PostToolUse",
                    &json!({"session_id": "sess-1", "tool_name": tool}),
                )
                .expect("test hook decodes")
                .lifecycle
                .unwrap()
                .signal,
            LifecycleSignal::ToolUsed {
                mutates,
                edits,
                native_key: None,
            }
        );
    }

    let stop = DroidAdapter
        .decode_hook("Stop", &json!({"session_id": "sess-1"}))
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    assert_eq!(
        stop.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false
        }
    );
    let prior = LifecycleState {
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        compacting: false,
    };
    assert_eq!(
        step(Some(&prior), None, &stop.signal).next.status,
        AgentStatus::Success
    );

    assert_eq!(
        DroidAdapter
            .decode_hook("PreCompact", &json!({"session_id": "sess-1"}))
            .expect("test hook decodes")
            .lifecycle
            .unwrap()
            .signal,
        LifecycleSignal::Compacting
    );
    assert_eq!(
        DroidAdapter
            .decode_hook(
                "SessionStart",
                &json!({"session_id": "sess-1", "source": "compact"}),
            )
            .expect("test hook decodes")
            .lifecycle
            .unwrap()
            .signal,
        LifecycleSignal::CompactionEnded { auto: None }
    );
    assert!(DroidAdapter.descriptor().ends_session("SessionEnd"));
    assert_eq!(
        DroidAdapter
            .decode_hook("SessionEnd", &json!({"session_id": "sess-1"}))
            .expect("test hook decodes")
            .lifecycle
            .unwrap()
            .signal,
        LifecycleSignal::Ended
    );
}

#[test]
fn transcript_v2_follows_active_chain_and_filters_private_blocks() {
    let messages = DroidAdapter.parse_transcript_messages(TRANSCRIPT_FIXTURE);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "ping");
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "pong\nsecond block");
    assert_eq!(
        messages[0].at.map(|at| at.to_string()).as_deref(),
        Some("2026-07-13T20:19:51.315Z")
    );
    assert!(messages.iter().all(|message| {
        !message.text.contains("hidden")
            && !message.text.contains("abandoned")
            && !message.text.contains("hook")
    }));
}

#[test]
fn transcript_v2_abstains_on_unknown_version_and_malformed_graphs() {
    let unknown = TRANSCRIPT_FIXTURE.replacen("\"version\":2", "\"version\":3", 1);
    assert!(DroidAdapter.parse_transcript_messages(&unknown).is_empty());

    let missing_parent = concat!(
        "{\"type\":\"session_start\",\"version\":2}\n",
        "{\"type\":\"message\",\"id\":\"a\",\"parentId\":\"missing\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"nope\"}]}}\n",
    );
    assert!(
        DroidAdapter
            .parse_transcript_messages(missing_parent)
            .is_empty()
    );

    let cycle = concat!(
        "{\"type\":\"session_start\",\"version\":2}\n",
        "{\"type\":\"message\",\"id\":\"a\",\"parentId\":\"b\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"one\"}]}}\n",
        "{\"type\":\"message\",\"id\":\"b\",\"parentId\":\"a\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"two\"}]}}\n",
    );
    assert!(DroidAdapter.parse_transcript_messages(cycle).is_empty());
}

#[test]
fn final_answer_and_identity_are_version_gated_and_bounded() {
    let (_dir, path) = transcript_fixture();
    let decoded = DroidAdapter
        .decode_hook(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "transcript_path": path,
            }),
        )
        .unwrap();
    let observation = decoded.lifecycle.as_ref().unwrap();

    assert_eq!(
        observation.launch.model.as_deref(),
        Some("custom:DeepSeek-V4-Pro-0")
    );
    assert_eq!(observation.launch.effort.as_deref(), Some("medium"));
    assert_eq!(observation.context_pct, None);
    assert_eq!(observation.context_window, None);
    assert_eq!(observation.total_tokens, None);
    assert_eq!(observation.fresh_input_tokens, None);
    assert_eq!(observation.output_tokens, None);
    assert_eq!(decoded.final_message, Some("pong\nsecond block".to_owned()));
    assert_eq!(
        DroidAdapter
            .decode_hook("SessionEnd", &Value::Null)
            .unwrap()
            .final_message,
        None
    );

    std::fs::remove_file(path.with_file_name("session.settings.json")).unwrap();
    let transcript = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!(
            "{transcript}{{\"type\":\"status\",\"message\":{{\"role\":\"assistant\",\"modelId\":\"bogus\",\"reasoningEffort\":\"low\"}}}}\n"
        ),
    )
    .unwrap();
    let fallback = DroidAdapter
        .decode_hook(
            "Stop",
            &json!({"session_id": "sess-1", "transcript_path": path}),
        )
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    assert_eq!(
        fallback.launch.model.as_deref(),
        Some("custom:fixture-model")
    );
    assert_eq!(fallback.launch.effort.as_deref(), Some("high"));
}

#[test]
fn settings_telemetry_keeps_root_cumulative_categories_out_of_context_truth() {
    let (_dir, path) = transcript_fixture();
    let refresh = transcript::telemetry(&path, None).unwrap();
    let usage = refresh.telemetry.session_usage.unwrap();

    assert_eq!(
        refresh.telemetry.model.as_deref(),
        Some("custom:DeepSeek-V4-Pro-0")
    );
    assert_eq!(usage.input_tokens, Some(18_007));
    assert_eq!(usage.output_tokens, Some(31));
    assert_eq!(usage.cache_creation_input_tokens, Some(2_400));
    assert_eq!(usage.cache_read_input_tokens, Some(91_000));
    assert_eq!(usage.thinking_tokens, Some(212));
    assert_eq!(usage.displayed_input_tokens(), 20_407);
    assert_eq!(usage.displayed_output_tokens(), 243);
    assert_eq!(usage.displayed_total_tokens(), 20_650);
    assert_eq!(usage.cache_read_tokens(), 91_000);
    assert!(refresh.telemetry.current_usage.is_none());
    assert!(refresh.telemetry.native_permission_wait.is_none());
    assert_eq!(
        transcript::telemetry(&refresh.settings_path, Some(&refresh.stat)),
        None,
        "the paired transcript/settings source is stat-gated"
    );
}

#[test]
fn transcript_ask_user_projects_and_clears_a_native_wait() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("ask.jsonl");
    let settings_path = dir.path().join("ask.settings.json");
    std::fs::write(
        &transcript_path,
        concat!(
            "{\"type\":\"session_start\",\"version\":2}\n",
            "{\"type\":\"message\",\"id\":\"user\",\"timestamp\":\"2026-07-14T15:13:51Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ask me\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"ask\",\"parentId\":\"user\",\"timestamp\":\"2026-07-14T15:13:55Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"AskUser\",\"input\":{\"questionnaire\":\"1. [question] Language?\"}}]}}\n",
        ),
    )
    .unwrap();
    std::fs::write(&settings_path, r#"{"model":"gpt-5"}"#).unwrap();

    let asking = transcript::telemetry(&transcript_path, None).unwrap();
    assert_eq!(
        asking.telemetry.native_permission_wait,
        Some("2026-07-14T15:13:55Z".parse().unwrap())
    );
    assert!(asking.stat.companion.is_some());

    let mut transcript = std::fs::read_to_string(&transcript_path).unwrap();
    transcript.push_str(
        "{\"type\":\"message\",\"id\":\"answer\",\"parentId\":\"ask\",\"timestamp\":\"2026-07-14T15:14:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"call-1\"}]}}\n",
    );
    std::fs::write(&transcript_path, transcript).unwrap();

    let answered = transcript::telemetry(&transcript_path, Some(&asking.stat)).unwrap();
    assert!(answered.telemetry.native_permission_wait.is_none());
}

#[test]
fn local_refresh_prices_exact_builtins_and_fills_gauge_from_last_call() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("priced.jsonl");
    let settings_path = dir.path().join("priced.settings.json");
    std::fs::write(
        &transcript_path,
        format!(
            "{{\"type\":\"session_start\",\"version\":2,\"cwd\":{}}}\n",
            serde_json::to_string(&dir.path().to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        &settings_path,
        r#"{"model":"gpt-5","reasoningEffort":"high","tokenUsage":{"inputTokens":100000,"outputTokens":20000,"cacheCreationTokens":10000,"cacheReadTokens":30000,"thinkingTokens":5000},"lastCallTokenUsage":{"inputTokens":6700,"outputTokens":825,"cacheCreationTokens":1200,"cacheReadTokens":56900}}"#,
    )
    .unwrap();
    let pricing_cache = dir.path().join("pricing-cache.json");
    std::fs::write(
        &pricing_cache,
        r#"{"schema":3,"litellm":{"gpt-5":{"input":0.00000125,"output":0.00001,"cache_read":0.000000125,"cache_create":0.00000125,"cache_read_explicit":true,"fast_multiplier":1.0,"max_input_tokens":400000}}}"#,
    )
    .unwrap();
    let transcript_text = transcript_path.to_string_lossy().into_owned();
    let ctx = LocalContextRefreshCtx {
        agent_id: "priced",
        model_hint: None,
        current_transcript_path: Some(&transcript_text),
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing_cache,
    };

    let refresh = DroidAdapter
        .local_context_refresh(RefreshTrigger::Hook("Stop"), &ctx)
        .unwrap();
    assert_eq!(
        refresh.context.model_id.as_set().map(String::as_str),
        Some("gpt-5")
    );
    let tokens = refresh.context.tokens.into_value().unwrap();
    assert_eq!(tokens.context_window_size, Some(400_000));
    assert!(tokens.used_percentage.is_none());
    assert!(tokens.remaining_percentage.is_none());
    let current = tokens.current_usage.unwrap();
    assert_eq!(current.input_tokens, Some(6_700));
    assert_eq!(current.output_tokens, Some(825));
    assert_eq!(current.cache_creation_input_tokens, Some(1_200));
    assert_eq!(current.cache_read_input_tokens, Some(56_900));
    assert_eq!(tokens.session_usage.unwrap().thinking_tokens, Some(5_000));
    let cost = refresh.context.cost.into_set().unwrap();
    assert_eq!(cost.coverage, crate::agents::CostCoverage::Session);
    assert!(cost.total_cost_usd.unwrap() > 0.0);
    assert_eq!(
        refresh.transcript_path.as_deref(),
        Some(transcript_path.to_string_lossy().as_ref())
    );
}

#[test]
fn suffix_streaming_is_exactly_once_torn_safe_and_resets_after_truncation() {
    let (_dir, path) = transcript_fixture();
    let path_text = path.to_string_lossy().into_owned();
    let mut cursor = TranscriptCursor::new(true);

    assert_eq!(
        cursor.messages(Some(&path_text), None, &DroidAdapter),
        ["abandoned answer", "pong\nsecond block"]
    );
    assert!(
        cursor
            .messages(Some(&path_text), None, &DroidAdapter)
            .is_empty()
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"{\"type\":\"message\",\"id\":\"new\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"fi")
        .unwrap();
    file.flush().unwrap();
    assert!(
        cursor
            .messages(Some(&path_text), None, &DroidAdapter)
            .is_empty()
    );
    file.write_all(b"nal\"}]}}\n").unwrap();
    file.flush().unwrap();
    assert_eq!(
        cursor.messages(Some(&path_text), None, &DroidAdapter),
        ["final"]
    );
    assert!(
        cursor
            .messages(Some(&path_text), None, &DroidAdapter)
            .is_empty()
    );

    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"session_start\",\"version\":2}\n",
            "{\"type\":\"message\",\"id\":\"fresh\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"fresh\"}]}}\n",
        ),
    )
    .unwrap();
    assert_eq!(
        cursor.messages(Some(&path_text), None, &DroidAdapter),
        ["fresh"]
    );
}

#[test]
fn transcript_positions_abstain_for_missing_or_unknown_headers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "{\"type\":\"session_start\",\"version\":9}\n").unwrap();

    assert_eq!(DroidAdapter.transcript_position(&path, None), None);
    assert_eq!(
        DroidAdapter.read_assistant_transcript_page(&path, None, TranscriptPosition::START),
        None
    );
}

#[test]
fn neutral_malformed_pid_and_launch_surfaces_are_explicit() {
    assert_eq!(
        DroidAdapter
            .decode_hook("Notification", &json!({}))
            .expect("test hook decodes")
            .class,
        AgentHookClass::Lifecycle
    );
    insta::assert_json_snapshot!(DroidAdapter.decode_hook("Stop", &Value::Null).expect("test hook decodes").neutral, @"null");
    insta::assert_json_snapshot!(
        DroidAdapter.decode_hook("Stop", &json!([])).expect("test hook decodes").lifecycle.unwrap(),
        @r###"
        {
          "signal": {
            "signal": "turn_ended",
            "errored": false,
            "parked_on_background": false
          }
        }
        "###
    );

    let descriptor = DroidAdapter.descriptor();
    assert!(descriptor.runs_as("droid"));
    assert!(descriptor.runs_as("droid-aarch64-unknown-linux-gnu"));
    assert!(!descriptor.runs_as("node"));
    assert_eq!(
        DroidAdapter.launch_command(&["--auto".to_owned(), "medium".to_owned()], Some("review")),
        Some(vec![
            "droid".to_owned(),
            "--auto".to_owned(),
            "medium".to_owned(),
            "--".to_owned(),
            "review".to_owned()
        ])
    );
    assert_eq!(
        DroidAdapter.resume_command("sess-1", Path::new("/tmp")),
        Some(vec![
            "droid".to_owned(),
            "--resume".to_owned(),
            "sess-1".to_owned()
        ])
    );
    assert_eq!(
        DroidAdapter.descriptor().launch.fork_command("sess-1"),
        Some(vec![
            "droid".to_owned(),
            "--fork".to_owned(),
            "sess-1".to_owned()
        ])
    );
    assert_eq!(
        DroidAdapter
            .descriptor()
            .launch
            .permission_args(PermissionMode::Auto),
        ["--auto", "medium"]
    );
    assert!(
        DroidAdapter
            .descriptor()
            .launch
            .permission_args(PermissionMode::Ask)
            .is_empty()
    );
    assert!(
        DroidAdapter
            .descriptor()
            .launch
            .permission_args(PermissionMode::Yolo)
            .is_empty()
    );
    assert_eq!(
        DroidAdapter
            .descriptor()
            .launch
            .permission_args(PermissionMode::Plan),
        ["--use-spec"]
    );

    assert_eq!(
        DroidAdapter.descriptor().render_preset(&LaunchPreset {
            append_system_prompt_file: Some(Path::new("/tmp/append.md").to_path_buf()),
            ..Default::default()
        }),
        Ok(vec![
            "--append-system-prompt-file".to_owned(),
            "/tmp/append.md".to_owned()
        ])
    );
    // Interactive Droid 0.171.0 has no `--model`/`--reasoning-effort`; both are
    // exec-only, so a profile that sets either fails fast rather than launching
    // with a silently ignored (and prompt-corrupting) flag.
    assert_eq!(
        DroidAdapter.descriptor().render_preset(&LaunchPreset {
            model: Some("glm-5".to_owned()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "droid",
            field: "model"
        })
    );
    assert_eq!(
        DroidAdapter.descriptor().render_preset(&LaunchPreset {
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "droid",
            field: "effort"
        })
    );
}
