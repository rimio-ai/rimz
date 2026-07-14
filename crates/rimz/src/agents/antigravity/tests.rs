use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::agents::{
    AgentStatus, LaunchPreset, StatusLineChange, TranscriptPosition, TranscriptRole, TurnPhase,
};

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

#[test]
fn safe_native_hooks_map_lifecycle_and_keep_pre_tool_policy_untouched() {
    let descriptor = AntigravityAdapter.descriptor();
    assert!(descriptor.capabilities.hook_install);
    assert!(!descriptor.capabilities.blocking_asks);
    assert_eq!(
        AntigravityAdapter.installed_hook_events(),
        INSTALLED_EVENT_LABELS
    );

    let common = json!({
        "conversationId": SESSION_ID,
        "workspacePaths": ["/workspace/project"],
        "transcriptPath": "/tmp/transcript.jsonl",
        "modelName": "Gemini 3.5 Flash",
    });
    for event in INSTALLED_EVENT_LABELS {
        assert_eq!(
            AntigravityAdapter.classify_hook(event, &common).class,
            AgentHookClass::Lifecycle
        );
    }

    let started = AntigravityAdapter
        .observe_lifecycle(
            "PreInvocation",
            &with(&common, [("invocationNum", json!(0))]),
        )
        .unwrap();
    assert_eq!(started.signal, LifecycleSignal::TurnStarted);
    assert_eq!(started.agent_id.as_deref(), Some(SESSION_ID));
    assert_eq!(started.worktree_path.as_deref(), Some("/workspace/project"));
    assert_eq!(
        started.transcript_path.as_deref(),
        Some("/tmp/transcript.jsonl")
    );
    assert_eq!(started.launch.model.as_deref(), Some("Gemini 3.5 Flash"));
    assert!(
        AntigravityAdapter
            .observe_lifecycle(
                "PreInvocation",
                &with(&common, [("invocationNum", json!(1))])
            )
            .is_none(),
        "later model calls in the same turn do not reopen its boundary"
    );

    for (event, error, expected) in [
        (
            "PostToolUse:edit",
            json!(""),
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
            },
        ),
        (
            "PostToolUse:mutating",
            json!(null),
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
            },
        ),
        (
            "PostToolUse:edit",
            json!("write failed"),
            LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
            },
        ),
    ] {
        let observed = AntigravityAdapter
            .observe_lifecycle(event, &with(&common, [("error", error)]))
            .unwrap();
        assert_eq!(observed.signal, expected);
    }

    let stopped = AntigravityAdapter
        .observe_lifecycle(
            "Stop",
            &with(
                &common,
                [
                    ("terminationReason", json!("model_stop")),
                    ("error", json!("")),
                    ("fullyIdle", json!(false)),
                ],
            ),
        )
        .unwrap();
    assert_eq!(
        stopped.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        }
    );
    let failed = AntigravityAdapter
        .observe_lifecycle(
            "Stop",
            &with(
                &common,
                [
                    ("terminationReason", json!("max_steps_exceeded")),
                    ("fullyIdle", json!(true)),
                ],
            ),
        )
        .unwrap();
    assert_eq!(
        failed.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }
    );

    let neutrals = [
        (
            "PreInvocation",
            AntigravityAdapter.render_neutral("PreInvocation").unwrap(),
        ),
        ("Stop", AntigravityAdapter.render_neutral("Stop").unwrap()),
    ];
    insta::assert_json_snapshot!(neutrals, @r###"
    [
      [
        "PreInvocation",
        {}
      ],
      [
        "Stop",
        {
          "decision": ""
        }
      ]
    ]
    "###);
    assert_eq!(
        AntigravityAdapter
            .classify_hook("PreToolUse", &common)
            .class,
        AgentHookClass::Unknown
    );
    assert_eq!(
        AntigravityAdapter.render_neutral("PreToolUse").unwrap(),
        None
    );
    insta::assert_debug_snapshot!(
        AntigravityAdapter.observe_lifecycle("Stop", &json!({"conversationId": SESSION_ID})),
        @"None"
    );
}

#[test]
fn hook_install_merges_both_files_and_uninstall_restores_the_statusline() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_path = dir.path().join("config/hooks.json");
    let settings_path = dir.path().join("antigravity-cli/settings.json");
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(
        &hooks_path,
        r#"{
  "mine": {
    "Stop": [{"type":"command","command":"my-stop","timeout":9}]
  }
}
"#,
    )
    .unwrap();
    let original_statusline = json!({
        "colorScheme": "tokyo night",
        "statusLine": {
            "type": "command",
            "command": "my-statusline --compact",
            "stack_with_default": false,
            "custom": "kept"
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&original_statusline).unwrap(),
    )
    .unwrap();

    let preview = install::preview(&hooks_path, &settings_path).unwrap();
    assert_eq!(preview.planned_events, INSTALLED_EVENT_LABELS);
    assert_eq!(preview.additional_configs.len(), 1);
    assert_eq!(preview.additional_configs[0].config_path, settings_path);
    assert_eq!(
        preview.status_line_change,
        Some(StatusLineChange::Wrapping {
            original: "my-statusline --compact".to_owned()
        })
    );
    assert!(!preview.candidate_config.contains("PreToolUse"));

    let report = install::install(&hooks_path, &settings_path).unwrap();
    assert_eq!(
        report.additional_config_paths,
        std::slice::from_ref(&settings_path)
    );
    assert!(install::installed(&hooks_path, &settings_path));
    assert_eq!(
        install::wrapped_statusline_command(&settings_path).as_deref(),
        Some("my-statusline --compact")
    );

    let mut hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks_path).unwrap()).unwrap();
    assert_eq!(
        hooks["mine"]["Stop"][0]["command"],
        Value::String("my-stop".to_owned())
    );
    assert!(hooks["rimz"].get("PreToolUse").is_none());
    assert_eq!(hooks["rimz"]["PreInvocation"].as_array().unwrap().len(), 1);
    assert_eq!(hooks["rimz"]["PostToolUse"].as_array().unwrap().len(), 3);

    hooks["rimz"]["Stop"][0]["timeout"] = json!(1);
    std::fs::write(&hooks_path, serde_json::to_string_pretty(&hooks).unwrap()).unwrap();
    assert!(!install::installed(&hooks_path, &settings_path));
    install::install(&hooks_path, &settings_path).unwrap();
    assert!(install::installed(&hooks_path, &settings_path));

    let once_hooks = std::fs::read_to_string(&hooks_path).unwrap();
    let once_settings = std::fs::read_to_string(&settings_path).unwrap();
    install::install(&hooks_path, &settings_path).unwrap();
    assert_eq!(std::fs::read_to_string(&hooks_path).unwrap(), once_hooks);
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        once_settings
    );

    let removed = install::uninstall(&hooks_path, &settings_path).unwrap();
    assert_eq!(removed.removed_events, INSTALLED_EVENT_LABELS);
    assert!(!install::managed(&hooks_path, &settings_path));
    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks_path).unwrap()).unwrap();
    assert!(hooks.get("rimz").is_none());
    assert_eq!(hooks["mine"]["Stop"][0]["command"], "my-stop");
    let restored: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(restored, original_statusline);
}

#[test]
fn hook_install_refuses_a_user_owned_rimz_hook_name() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_path = dir.path().join("hooks.json");
    let settings_path = dir.path().join("settings.json");
    std::fs::write(
        &hooks_path,
        r#"{"rimz":{"Stop":[{"type":"command","command":"user-command"}]}}"#,
    )
    .unwrap();
    let error = install::preview(&hooks_path, &settings_path).unwrap_err();
    assert!(error.to_string().contains("hook name `rimz`"));
    assert!(error.to_string().contains("user-owned"));
}

#[test]
fn added_statusline_stacks_with_default_and_uninstall_removes_only_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_path = dir.path().join("hooks.json");
    let settings_path = dir.path().join("settings.json");
    std::fs::write(&settings_path, r#"{"colorScheme":"tokyo night"}"#).unwrap();

    install::install(&hooks_path, &settings_path).unwrap();
    let installed: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(installed["statusLine"]["stack_with_default"], true);
    assert_eq!(installed["statusLine"]["command"], STATUS_LINE_COMMAND);

    install::uninstall(&hooks_path, &settings_path).unwrap();
    let restored: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(restored, json!({"colorScheme": "tokyo night"}));
}

#[test]
fn statusline_projects_model_account_and_context_usage() {
    let context = AntigravityAdapter
        .observe_context(
            "antigravity",
            &json!({
                "conversation_id": SESSION_ID,
                "version": "1.1.2",
                "model": {"id": "gemini-3.5-flash", "display_name": "Gemini 3.5 Flash"},
                "plan_tier": "ultra",
                "email": "user@example.com",
                "tool_confirmation_pending": true,
                "context_window": {
                    "context_window_size": 1_048_576,
                    "used_percentage": 8.4156,
                    "remaining_percentage": 91.5844,
                    "current_usage": {
                        "input_tokens": 63_382,
                        "output_tokens": 346,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 20_857
                    }
                },
                "future_field": {"ignored": true}
            }),
        )
        .unwrap();
    assert_eq!(context.model_id.as_deref(), Some("gemini-3.5-flash"));
    assert_eq!(
        context.model_display_name.as_deref(),
        Some("Gemini 3.5 Flash")
    );
    assert_eq!(context.agent_version.as_deref(), Some("1.1.2"));
    assert!(context.native_permission_wait.is_some());
    let account = context.account.unwrap();
    assert_eq!(account.plan.as_deref(), Some("ultra"));
    assert_eq!(account.account_id.as_deref(), Some("user@example.com"));
    let tokens = context.tokens.unwrap();
    assert_eq!(tokens.context_window_size, Some(1_048_576));
    assert_eq!(tokens.used_percentage, Some(8));
    assert_eq!(tokens.remaining_percentage, Some(92));
    assert_eq!(
        tokens.current_usage.unwrap().cache_read_input_tokens,
        Some(20_857)
    );
    assert!(
        AntigravityAdapter
            .observe_context("antigravity", &json!({"tool_confirmation_pending": false}))
            .unwrap()
            .native_permission_wait
            .is_none()
    );
}

#[test]
fn verified_visible_transcript_records_are_normalized_strictly() {
    let transcript = include_str!("tests/fixtures/transcript.jsonl");
    let messages = AntigravityAdapter.parse_transcript_messages(transcript);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "ping");
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "pong");
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("checkpoint"))
    );

    assert!(
        AntigravityAdapter
            .parse_transcript_messages("not-json\n{}")
            .is_empty()
    );
    assert!(
        AntigravityAdapter
            .parse_transcript_messages(
                r#"{"step_index":4,"source":"MODEL","type":"PLANNER_THOUGHT","status":"DONE","created_at":"2026-07-13T23:23:10Z","content":"hidden"}"#,
            )
            .is_empty()
    );
    assert!(
        AntigravityAdapter
            .parse_transcript_messages(
                r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"IN_PROGRESS","created_at":"2026-07-13T23:23:10Z","content":"partial"}"#,
            )
            .is_empty()
    );
}

#[test]
fn transcript_cursor_retains_a_torn_final_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(
        b"{\"step_index\":0,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:09Z\",\"content\":\"one\"}\n{\"step_index\":1",
    )
    .unwrap();
    let page = AntigravityAdapter
        .read_assistant_transcript_page(&path, None, TranscriptPosition::START)
        .unwrap();
    assert_eq!(page.messages, ["one"]);
    let next = page.next;
    file.write_all(
        b",\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:10Z\",\"content\":\"two\"}",
    )
    .unwrap();
    let page = AntigravityAdapter
        .read_assistant_transcript_page(&path, None, next)
        .unwrap();
    assert_eq!(page.messages, ["two"]);
}

#[test]
fn discovery_uses_cache_only_for_fresh_pairing_and_keeps_exact_resume_available() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let transcript = write_transcript(dir.path(), SESSION_ID);
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::fs::write(
        dir.path().join("cache/last_conversations.json"),
        format!(
            "{{{}:{}}}",
            serde_json::to_string(&workspace).unwrap(),
            serde_json::to_string(SESSION_ID).unwrap()
        ),
    )
    .unwrap();

    let observations = session::discover_under(dir.path(), &workspace);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation.session_id.as_str(), SESSION_ID);
    assert_eq!(observation.transcript_path, transcript);
    assert_eq!(observation.status, AgentStatus::Success);
    assert_eq!(observation.phase, TurnPhase::Idle);
    assert_eq!(observation.latest_prompt.as_deref(), Some("ping"));
    assert!(observation.first_event_at.is_some());

    let other_workspace = dir.path().join("other");
    std::fs::create_dir(&other_workspace).unwrap();
    let observations = session::discover_under(dir.path(), &other_workspace);
    assert_eq!(observations.len(), 1);
    assert!(
        observations[0].first_event_at.is_none(),
        "an unrelated workspace can bind this record only by exact resume id"
    );
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_conversation_directories() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let escaped = tempfile::tempdir().unwrap();
    let escaped_transcript = escaped
        .path()
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(escaped_transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &escaped_transcript,
        include_str!("tests/fixtures/transcript.jsonl"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("brain")).unwrap();
    symlink(escaped.path(), dir.path().join("brain").join(SESSION_ID)).unwrap();
    assert!(session::discover_under(dir.path(), Path::new("/workspace/project")).is_empty());
}

#[test]
fn launch_resume_permissions_and_model_preset_match_agy_1_1_2() {
    assert_eq!(SUPPORTED_VERSION, "1.1.2");
    assert_eq!(
        AntigravityAdapter.launch_command(&["--sandbox".to_owned()], Some("review")),
        Some(vec![
            "agy".to_owned(),
            "--sandbox".to_owned(),
            "--prompt-interactive".to_owned(),
            "review".to_owned(),
        ])
    );
    assert_eq!(
        AntigravityAdapter.resume_command(SESSION_ID, Path::new("/workspace/project")),
        Some(vec![
            "agy".to_owned(),
            "--conversation".to_owned(),
            SESSION_ID.to_owned(),
        ])
    );
    assert_eq!(
        AntigravityAdapter.permission_args(PermissionMode::Auto),
        ["--mode", "accept-edits"]
    );
    assert_eq!(
        AntigravityAdapter.permission_args(PermissionMode::Plan),
        ["--mode", "plan"]
    );
    assert_eq!(
        AntigravityAdapter.permission_args(PermissionMode::Yolo),
        ["--dangerously-skip-permissions"]
    );
    assert_eq!(
        AntigravityAdapter.render_preset(&LaunchPreset {
            model: Some("Gemini 3.5 Flash (Low)".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "Gemini 3.5 Flash (Low)".to_owned(),
        ])
    );
    assert!(
        AntigravityAdapter
            .render_preset(&LaunchPreset {
                effort: Some("high".to_owned()),
                ..Default::default()
            })
            .is_err()
    );
    assert!(AntigravityAdapter.compact_command().is_none());
    assert!(
        AntigravityAdapter
            .fork_command(SESSION_ID, Path::new("/workspace"))
            .is_none()
    );
}

#[test]
fn exact_resume_parser_accepts_both_flag_forms_without_claiming_continue() {
    for command in [
        "agy --conversation 11111111-1111-4111-8111-111111111111",
        "/home/user/.local/bin/agy --conversation=11111111-1111-4111-8111-111111111111",
    ] {
        assert_eq!(
            AntigravityAdapter
                .resumed_session_id_from_cmdline(command)
                .as_deref(),
            Some(SESSION_ID)
        );
    }
    for command in [
        "agy --continue",
        "agy -c",
        "agy --conversation=",
        "agy --conversation --mode plan",
        "echo agy --conversation 11111111-1111-4111-8111-111111111111",
    ] {
        assert!(
            AntigravityAdapter
                .resumed_session_id_from_cmdline(command)
                .is_none()
        );
    }
}

#[test]
fn home_resolution_and_presence_are_exact() {
    assert_eq!(
        session::resolve_home(Some(OsStr::new("/tmp/agy")), Some(OsStr::new("/home/user"))),
        Some(Path::new("/tmp/agy").to_path_buf())
    );
    assert_eq!(
        session::resolve_home(None, Some(OsStr::new("/home/user"))),
        Some(Path::new("/home/user/.gemini/antigravity-cli").to_path_buf())
    );
    assert!(AntigravityAdapter.descriptor().runs_as("agy"));
    assert!(!AntigravityAdapter.descriptor().runs_as("antigravity"));
}

fn write_transcript(home: &Path, session_id: &str) -> std::path::PathBuf {
    let path = home
        .join("brain")
        .join(session_id)
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, include_str!("tests/fixtures/transcript.jsonl")).unwrap();
    path
}

fn with<const N: usize>(base: &Value, fields: [(&str, Value); N]) -> Value {
    let mut value = base.clone();
    let object = value.as_object_mut().unwrap();
    for (key, field) in fields {
        object.insert(key.to_owned(), field);
    }
    value
}
