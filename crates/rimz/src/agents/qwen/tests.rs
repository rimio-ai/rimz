use std::fs;
use std::io::Write as _;

use serde_json::{Value, json};

use super::*;
use crate::agents::transcript::TranscriptCursor;
use crate::agents::{AgentHookClass, AskKind};

const REWOUND_SESSION: &str = include_str!("tests/fixtures/rewound-session.jsonl");

#[test]
fn classifies_native_asks_and_keeps_neutral_stdout_silent() {
    let adapter = QwenAdapter;
    let permission = adapter.classify_hook(
        "PermissionRequest",
        &json!({"tool_name":"run_shell_command"}),
    );
    assert_eq!(permission.class, AgentHookClass::AwaitingUser);
    assert_eq!(permission.ask_kind, Some(AskKind::Permission));
    let question = adapter.classify_hook("PreToolUse", &json!({"tool_name":"ask_user_question"}));
    assert_eq!(question.ask_kind, Some(AskKind::Question));
    insta::assert_debug_snapshot!(adapter.render_neutral("PermissionRequest").unwrap(), @"None");
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
        adapter.permission_args(PermissionMode::Auto),
        ["--approval-mode", "auto-edit"]
    );
    assert_eq!(adapter.compact_command(), Some("/compress"));
}

#[test]
fn installs_restores_and_leaves_preset_statusline_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, r#"{"ui":{"statusLine":{"type":"command","command":"myline","refreshInterval":5}},"theme":"dark"}"#).unwrap();
    let report = install::install_into(&path).unwrap();
    assert_eq!(report.installed_events.len(), 14);
    assert!(install::hooks_installed_at(&path));
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
    assert_eq!(
        installed
            .pointer("/hooks/PreToolUse/0/hooks/0/timeout")
            .and_then(Value::as_u64),
        Some(10_000)
    );
    assert_eq!(
        installed
            .pointer("/hooks/PreToolUse/0/matcher")
            .and_then(Value::as_str),
        Some(BLOCKING_TOOL_MATCHER)
    );
    install::uninstall_from(&path).unwrap();
    assert!(!install::hooks_installed_at(&path));
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
    let preview = install::preview_install_at(&path).unwrap();
    assert_eq!(preview.status_line_change, None);
    let candidate: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    assert_eq!(
        candidate
            .pointer("/ui/statusLine/type")
            .and_then(Value::as_str),
        Some("preset")
    );
}

#[test]
fn rejects_async_managed_blocking_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, format!(r#"{{"hooks":{{"PermissionRequest":[{{"_rimz_managed":true,"hooks":[{{"type":"command","command":"{RIMZ_HOOK_COMMAND}","async":true}}]}}]}}}}"#)).unwrap();
    let error = install::install_into(&path).unwrap_err().to_string();
    assert!(error.contains("async"));

    fs::write(&path, format!(r#"{{"hooks":{{"PreToolUse":[{{"_rimz_managed":true,"hooks":[{{"type":"command","command":"{RIMZ_HOOK_COMMAND}","async":true}}]}}]}}}}"#)).unwrap();
    let error = install::install_into(&path).unwrap_err().to_string();
    assert!(error.contains("PreToolUse"));
}

#[test]
fn maps_lifecycle_context_background_and_subagents() {
    let adapter = QwenAdapter;
    let start = adapter
        .observe_lifecycle(
            "SessionStart",
            &json!({"session_id":"s1","source":"startup","model":"qwen3"}),
        )
        .unwrap();
    assert_eq!(start.signal, LifecycleSignal::Registered);
    assert_eq!(start.origin, Some(SessionOrigin::Fresh));
    let tool = adapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({"session_id":"s1","tool_name":"write_file"}),
        )
        .unwrap();
    assert_eq!(
        tool.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true
        }
    );
    let parked = adapter.observe_lifecycle("Stop", &json!({"session_id":"s1","background_tasks":[{"status":"running"}],"context_usage":1.2,"context_limit":131072})).unwrap();
    assert_eq!(
        parked.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true
        }
    );
    assert_eq!(parked.context_pct, Some(100));
    assert_eq!(parked.context_window, Some(131072));
    let child = adapter
        .observe_lifecycle(
            "SubagentStart",
            &json!({"session_id":"parent","agent_id":"child","agent_type":"review"}),
        )
        .unwrap();
    assert_eq!(child.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(child.task.as_deref(), Some("review"));
    assert!(child.parent_agent_id.is_some());
    assert_eq!(
        adapter
            .observe_lifecycle(
                "SessionStart",
                &json!({"session_id":"s1","source":"compact"})
            )
            .unwrap()
            .signal,
        LifecycleSignal::CompactionEnded { auto: None }
    );
}

#[test]
fn transcript_tail_and_statusline_supply_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1.jsonl");
    fs::write(&path, "{\"type\":\"assistant\",\"model\":\"qwen3\",\"contextWindowSize\":131072,\"usageMetadata\":{\"totalTokenCount\":420}}\n").unwrap();
    let adapter = QwenAdapter;
    let observation = adapter
        .observe_lifecycle("Stop", &json!({"session_id":"s1","transcript_path":path}))
        .unwrap();
    assert_eq!(observation.total_tokens, Some(420));
    assert_eq!(observation.context_window, Some(131072));

    let context = adapter.observe_context("qwen", &json!({
        "version":"0.19.8",
        "model":{"display_name":"qwen3-coder-plus"},
        "context_window":{"context_window_size":131072,"used_percentage":34.3,"current_usage":45000},
        "metrics":{"models":{"qwen3":{"tokens":{"prompt":30000,"completion":5000,"cached":10000,"thoughts":2000}}},"files":{"total_lines_added":12,"total_lines_removed":3}},
        "vim":{"mode":"INSERT"}
    })).unwrap();
    assert_eq!(
        context.model_display_name.as_deref(),
        Some("qwen3-coder-plus")
    );
    assert_eq!(
        context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(34)
    );
    let tokens = context.tokens.as_ref().unwrap();
    assert_eq!(tokens.used_tokens(), Some(45_000));
    assert_eq!(tokens.current_usage.as_ref().unwrap().output_tokens, None);
    assert_eq!(
        context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_lines_added),
        Some(12)
    );
}

#[test]
fn rewound_transcript_supplies_active_hook_boundary_usage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rewound-session.jsonl");
    fs::write(&path, REWOUND_SESSION).unwrap();
    let adapter = QwenAdapter;
    for event in ["SessionStart", "Stop"] {
        let observation = adapter
            .observe_lifecycle(
                event,
                &json!({
                    "hook_event_name": event,
                    "session_id": "sess-rewind",
                    "source": "startup",
                    "transcript_path": path,
                    "input_tokens": 12
                }),
            )
            .unwrap();
        assert_eq!(
            observation.launch.model.as_deref(),
            Some("qwen-active-final")
        );
        assert_eq!(observation.total_tokens, Some(555));
        assert_eq!(observation.context_window, Some(333_333));
    }
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
            .messages(Some(&path_text), None, &QwenAdapter)
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
        cursor.messages(Some(&path_text), None, &QwenAdapter),
        ["replacement answer"]
    );
    assert!(
        cursor
            .messages(Some(&path_text), None, &QwenAdapter)
            .is_empty()
    );

    let mut from_start = TranscriptCursor::new(true);
    assert_eq!(
        from_start.messages(Some(&path_text), None, &QwenAdapter),
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
        adapter
            .observe_turn_error_from_hook("StopFailure", &json!({"error":"rate_limit"}))
            .unwrap()
            .class,
        TurnErrorClass::PausedRateLimit
    );
    assert_eq!(
        adapter
            .observe_turn_error_from_hook("StopFailure", &json!({"error":"server_error"}))
            .unwrap()
            .class,
        TurnErrorClass::PausedOverloaded
    );
}
