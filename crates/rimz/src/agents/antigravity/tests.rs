use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::agents::{
    AgentStatus, LaunchPreset, PriceBook, StatusLineChange, TranscriptPosition, TranscriptRole,
    TurnPhase,
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
        "transcriptPath": "/tmp/transcript_full.jsonl",
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
        Some("/tmp/transcript_full.jsonl")
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
    assert_eq!(preview.files.len(), 2);
    assert_eq!(preview.files[1].path, settings_path);
    assert_eq!(
        preview.status_line_change,
        Some(StatusLineChange::Wrapping {
            original: "my-statusline --compact".to_owned()
        })
    );
    assert!(!preview.files[0].candidate.contains("PreToolUse"));

    let report = install::install(&hooks_path, &settings_path).unwrap();
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.files[1].path, settings_path);
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
                "model": {
                    "id": "Gemini 3.5 Flash (Medium)",
                    "display_name": "Gemini 3.5 Flash (Medium)"
                },
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
    assert_eq!(
        context.model_id.as_deref(),
        Some("Gemini 3.5 Flash (Medium)")
    );
    assert_eq!(
        context.model_display_name.as_deref(),
        Some("Gemini 3.5 Flash")
    );
    assert_eq!(context.agent_version.as_deref(), Some("1.1.2"));
    assert!(context.native_permission_wait.is_some());
    let account = context.account.unwrap();
    assert_eq!(account.plan.as_deref(), Some("ultra"));
    assert_eq!(account.account_id.as_deref(), Some("user@example.com"));
    assert_eq!(account.metered, Some(true));
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
fn statusline_normalizes_captured_reasoning_qualifiers_and_preserves_model_id() {
    for (display, expected_model, expected_effort, thinking) in [
        (
            "Gemini 3.5 Flash (Medium)",
            "Gemini 3.5 Flash",
            Some("medium"),
            None,
        ),
        (
            "Gemini 3.5 Flash (High)",
            "Gemini 3.5 Flash",
            Some("high"),
            None,
        ),
        (
            "Gemini 3.5 Flash (Low)",
            "Gemini 3.5 Flash",
            Some("low"),
            None,
        ),
        ("Gemini 3.1 Pro (Low)", "Gemini 3.1 Pro", Some("low"), None),
        (
            "Gemini 3.1 Pro (High)",
            "Gemini 3.1 Pro",
            Some("high"),
            None,
        ),
        (
            "Claude Sonnet 4.6 (Thinking)",
            "Claude Sonnet 4.6",
            None,
            Some(true),
        ),
        (
            "Claude Opus 4.6 (Thinking)",
            "Claude Opus 4.6",
            None,
            Some(true),
        ),
        (
            "GPT-OSS 120B (Medium)",
            "GPT-OSS 120B",
            Some("medium"),
            None,
        ),
    ] {
        let context = AntigravityAdapter
            .observe_context(
                "antigravity",
                &json!({
                    "model": {
                        "id": "  provider/model:id  ",
                        "display_name": display,
                    }
                }),
            )
            .unwrap();
        assert_eq!(context.model_id.as_deref(), Some("  provider/model:id  "));
        assert_eq!(context.model_display_name.as_deref(), Some(expected_model));
        assert_eq!(context.effort.as_deref(), expected_effort);
        assert_eq!(context.thinking_enabled, thinking);
    }

    for (display, expected_model, effort, thinking) in [
        (
            "  Gemini 3.5 Flash (mEdIuM) \t",
            "Gemini 3.5 Flash",
            Some("medium"),
            None,
        ),
        (
            "Claude Sonnet 4.6 (THINKING)",
            "Claude Sonnet 4.6",
            None,
            Some(true),
        ),
        (
            "Gemini 3.5 Flash (Turbo)",
            "Gemini 3.5 Flash (Turbo)",
            None,
            None,
        ),
        (
            "Gemini 3.5 Flash (Low) preview",
            "Gemini 3.5 Flash (Low) preview",
            None,
            None,
        ),
        ("Gemini 3.5 Flash (Low", "Gemini 3.5 Flash (Low", None, None),
    ] {
        let context = AntigravityAdapter
            .observe_context("antigravity", &json!({"model": {"display_name": display}}))
            .unwrap();
        assert_eq!(context.model_display_name.as_deref(), Some(expected_model));
        assert_eq!(context.effort.as_deref(), effort);
        assert_eq!(context.thinking_enabled, thinking);
    }
}

#[test]
fn statusline_prices_canonical_ids_and_observed_selector_labels_with_usage() {
    let prices = PriceBook::from_litellm_json(
        r#"{
            "agy-priced": {
                "input_cost_per_token": 1e-6,
                "output_cost_per_token": 2e-6,
                "cache_creation_input_token_cost": 3e-6,
                "cache_read_input_token_cost": 0.25e-6
            },
            "gemini-3.5-flash": {
                "input_cost_per_token": 1.5e-6,
                "output_cost_per_token": 9e-6,
                "cache_creation_input_token_cost": 1.875e-6,
                "cache_read_input_token_cost": 0.15e-6
            }
        }"#,
    );
    let payload = json!({
        "model": {
            "id": "  agy-priced-via-router  ",
            "display_name": "unpriceable display label"
        },
        "context_window": {
            "current_usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "cache_creation_input_tokens": 30,
                "cache_read_input_tokens": 40
            }
        }
    });
    let cost = AntigravityAdapter
        .estimate_context_cost(&payload, &prices)
        .unwrap();
    assert_eq!(cost.basis, crate::agents::CostBasis::DisplayEstimate);
    let total_cost_usd = cost.total_cost_usd.unwrap();
    assert!((total_cost_usd - 150e-6).abs() < 1e-15);
    assert!(total_cost_usd.is_finite() && total_cost_usd > 0.0);

    let captured = json!({
        "model": {
            "id": "Gemini 3.5 Flash (Medium)",
            "display_name": "Gemini 3.5 Flash (Medium)"
        },
        "context_window": {
            "current_usage": {
                "input_tokens": 2_971,
                "output_tokens": 630,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 16_270
            }
        }
    });
    let captured_cost = AntigravityAdapter
        .estimate_context_cost(&captured, &prices)
        .unwrap();
    assert_eq!(
        captured_cost.basis,
        crate::agents::CostBasis::DisplayEstimate
    );
    assert!((captured_cost.total_cost_usd.unwrap() - 0.012_567).abs() < 1e-15);

    for payload in [
        json!({
            "model": {"id": "unknown", "display_name": "agy-priced"},
            "context_window": {"current_usage": {"input_tokens": 10}}
        }),
        json!({"model": {"id": "agy-priced"}}),
        json!({
            "model": {"id": "agy-priced"},
            "context_window": {"current_usage": {"input_tokens": 0}}
        }),
        json!({
            "model": {"id": "   ", "display_name": "agy-priced"},
            "context_window": {"current_usage": {"input_tokens": 10}}
        }),
        json!({
            "model": {"id": "Gemini 3.5 Flash Turbo (Medium)"},
            "context_window": {"current_usage": {"input_tokens": 10}}
        }),
    ] {
        assert!(
            AntigravityAdapter
                .estimate_context_cost(&payload, &prices)
                .is_none()
        );
    }
}

#[test]
fn verified_visible_transcript_records_are_normalized_strictly() {
    let transcript = include_str!("tests/fixtures/transcript_full.jsonl");
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
fn transcript_user_envelopes_expose_only_the_request_body() {
    let wrapped = "  <USER_REQUEST>\nrefine the card\nwithout metadata\n</USER_REQUEST>\n<ADDITIONAL_METADATA>{\"secret\":true}</ADDITIONAL_METADATA>\n<SETTINGS>ignored</SETTINGS>  ";
    let lines = [
        user_record(0, "2026-07-13T23:23:09Z", wrapped),
        user_record(1, "2026-07-13T23:23:10Z", "<USER_REQUEST>missing close"),
        user_record(
            2,
            "2026-07-13T23:23:11Z",
            "<USER_REQUEST>  \n </USER_REQUEST><ADDITIONAL_METADATA />",
        ),
        user_record(3, "2026-07-13T23:23:12Z", "  legacy prompt  "),
    ]
    .join("\n");
    assert_eq!(
        session::messages(&lines)
            .into_iter()
            .map(|message| message.text)
            .collect::<Vec<_>>(),
        ["refine the card\nwithout metadata", "legacy prompt"]
    );

    let dir = tempfile::tempdir().unwrap();
    let path = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &user_record(0, "2026-07-13T23:23:09Z", wrapped),
    );
    assert_eq!(
        session::latest_prompt_under(dir.path(), &path, SESSION_ID).as_deref(),
        Some("refine the card\nwithout metadata")
    );

    for (content, expected) in [
        ("<USER_REQUEST>missing close", None),
        ("<USER_REQUEST> </USER_REQUEST><SETTINGS />", None),
        ("  legacy prompt  ", Some("legacy prompt")),
    ] {
        std::fs::write(&path, user_record(0, "2026-07-13T23:23:09Z", content)).unwrap();
        assert_eq!(
            session::latest_prompt_under(dir.path(), &path, SESSION_ID).as_deref(),
            expected
        );
    }
}

#[test]
fn transcript_questions_project_native_waits_and_clear_on_progress() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let path = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &[
            user_record(0, "2026-07-13T23:23:09Z", "start"),
            planner_record(
                1,
                "2026-07-13T23:23:10Z",
                Some(json!([{
                    "name": "ask_question",
                    "args": {
                        "questions": [
                            {"question": "  First choice?  ", "options": ["a", "b"], "future": true},
                            {"question": "Second choice?", "options": []}
                        ]
                    },
                    "future": "ignored"
                }])),
            ),
        ]
        .join("\n"),
    );
    let observation = &session::discover_under(dir.path(), workspace)[0];
    assert_eq!(observation.status, AgentStatus::Waiting);
    assert_eq!(observation.phase, TurnPhase::Idle);
    assert_eq!(
        observation.native_prompt_detail.as_deref(),
        Some("First choice?")
    );
    assert_eq!(
        observation.waiting_since,
        Some("2026-07-13T23:23:10Z".parse().unwrap())
    );

    let legacy_questions = serde_json::to_string(&json!([
        {"question": "Replacement?", "options": ["yes", "no"]}
    ]))
    .unwrap();
    let replacement = planner_record(
        2,
        "2026-07-13T23:23:11Z",
        Some(json!([{
            "name": "ask_question",
            "args": {"questions": legacy_questions}
        }])),
    );
    let mut lines = std::fs::read_to_string(&path).unwrap();
    lines.push('\n');
    lines.push_str(&replacement);
    std::fs::write(&path, &lines).unwrap();
    let observation = &session::discover_under(dir.path(), workspace)[0];
    assert_eq!(
        observation.native_prompt_detail.as_deref(),
        Some("Replacement?")
    );
    assert_eq!(
        observation.waiting_since,
        Some("2026-07-13T23:23:11Z".parse().unwrap())
    );

    lines.push('\n');
    lines.push_str(&planner_record(3, "2026-07-13T23:23:12Z", None));
    std::fs::write(&path, &lines).unwrap();
    let observation = &session::discover_under(dir.path(), workspace)[0];
    assert_eq!(observation.status, AgentStatus::Success);
    assert!(observation.native_prompt_detail.is_none());
    assert!(observation.waiting_since.is_none());

    lines.push('\n');
    lines.push_str(&planner_record(
        4,
        "2026-07-13T23:23:13Z",
        Some(json!([{
            "name": "ask_question",
            "args": {"questions": [{"question": "One more?"}]}
        }])),
    ));
    lines.push('\n');
    lines.push_str(&user_record(5, "2026-07-13T23:23:14Z", "continue"));
    std::fs::write(&path, &lines).unwrap();
    let observation = &session::discover_under(dir.path(), workspace)[0];
    assert_eq!(observation.status, AgentStatus::Running);
    assert_eq!(observation.phase, TurnPhase::Reasoning);
    assert!(observation.native_prompt_detail.is_none());
    assert!(observation.waiting_since.is_none());
}

#[test]
fn malformed_or_empty_question_calls_settle_as_ordinary_responses() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    for tool_calls in [
        json!("not-an-array"),
        json!([{"name": "ask_question", "args": {"questions": "not-json"}}]),
        json!([{"name": "ask_question", "args": {"questions": []}}]),
        json!([{"name": "ask_question", "args": {"questions": [null, {}, {"question": "  "}]}}]),
        json!([{"name": "another_tool", "args": {"questions": [{"question": "ignored"}]}}]),
    ] {
        write_transcript_named(
            dir.path(),
            SESSION_ID,
            "transcript_full.jsonl",
            &[
                user_record(0, "2026-07-13T23:23:09Z", "start"),
                planner_record(1, "2026-07-13T23:23:10Z", Some(tool_calls)),
            ]
            .join("\n"),
        );
        let observation = &session::discover_under(dir.path(), workspace)[0];
        assert_eq!(observation.status, AgentStatus::Success);
        assert!(observation.native_prompt_detail.is_none());
        assert!(observation.waiting_since.is_none());
    }
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
    assert_eq!(observation.fresh_binding_at, observation.first_event_at);

    let other_workspace = dir.path().join("other");
    std::fs::create_dir(&other_workspace).unwrap();
    let observations = session::discover_under(dir.path(), &other_workspace);
    assert_eq!(observations.len(), 1);
    assert!(observations[0].first_event_at.is_some());
    assert!(
        observations[0].fresh_binding_at.is_none(),
        "an unrelated workspace can bind this record only by exact resume id"
    );
}

#[test]
fn transcript_selection_accepts_both_names_and_prefers_full_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let full = write_transcript(dir.path(), SESSION_ID);
    let legacy = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript.jsonl",
        &include_str!("tests/fixtures/transcript_full.jsonl").replace("ping", "legacy prompt"),
    );

    assert_eq!(
        session::latest_prompt_under(dir.path(), &full, SESSION_ID).as_deref(),
        Some("ping")
    );
    assert_eq!(
        session::latest_prompt_under(dir.path(), &legacy, SESSION_ID).as_deref(),
        Some("legacy prompt")
    );
    let observations = session::discover_under(dir.path(), &workspace);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].transcript_path, full);
    assert_eq!(observations[0].latest_prompt.as_deref(), Some("ping"));

    let unknown = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_debug.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    assert!(session::latest_prompt_under(dir.path(), &unknown, SESSION_ID).is_none());
    assert!(session::latest_prompt_under(dir.path(), &full, "other-conversation").is_none());
}

#[test]
fn first_invocation_recovers_only_the_latest_completed_visible_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        concat!(
            "not-json\n",
            "{\"step_index\":0,\"source\":\"SYSTEM\",\"type\":\"CHECKPOINT\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:08Z\",\"content\":\"internal description\"}\n",
            "{\"step_index\":1,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:09Z\",\"content\":\"<command-name>synthetic\"}\n",
            "{\"step_index\":2,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"IN_PROGRESS\",\"created_at\":\"2026-07-13T23:23:10Z\",\"content\":\"partial description\"}\n",
            "{\"step_index\":3,\"source\":\"MODEL\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:11Z\",\"content\":\"wrong source\"}\n",
            "{\"step_index\":4,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:12Z\",\"content\":\"  label this turn  \"}\n",
            "{\"step_index\":5,\"source\":\"USER_EXPLICIT\"",
        ),
    );
    let payload = json!({
        "conversationId": SESSION_ID,
        "workspacePaths": ["/workspace/project"],
        "transcriptPath": transcript,
        "invocationNum": 0,
    });

    let started = observe_lifecycle_with_prompt_reader("PreInvocation", &payload, |path, id| {
        session::latest_prompt_under(dir.path(), path, id)
    })
    .unwrap();
    assert_eq!(started.prompt.as_deref(), Some("label this turn"));

    let stopped = observe_lifecycle_with_prompt_reader(
        "Stop",
        &with(
            &payload,
            [
                ("fullyIdle", json!(true)),
                ("terminationReason", json!("model_stop")),
            ],
        ),
        |_, _| panic!("later lifecycle events do not read transcripts"),
    )
    .unwrap();
    assert!(stopped.prompt.is_none());

    let unknown = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_debug.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    let rejected = observe_lifecycle_with_prompt_reader(
        "PreInvocation",
        &with(&payload, [("transcriptPath", json!(unknown))]),
        |path, id| session::latest_prompt_under(dir.path(), path, id),
    )
    .unwrap();
    assert!(rejected.prompt.is_none());
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_conversation_directories() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let escaped = tempfile::tempdir().unwrap();
    let escaped_transcript = escaped
        .path()
        .join(".system_generated/logs/transcript_full.jsonl");
    std::fs::create_dir_all(escaped_transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &escaped_transcript,
        include_str!("tests/fixtures/transcript_full.jsonl"),
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

mod local_account_api {
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use jiff::Timestamp;
    use serde_json::json;

    use super::super::local_api::{self, LoopbackEndpoint};
    use super::*;

    const OBSERVED_AT: &str = "2026-06-15T08:00:00Z";
    const FIVE_HOUR_RESET: &str = "2026-06-15T12:00:00Z";
    const WEEK_RESET: &str = "2026-06-20T08:00:00Z";

    fn observed_at() -> Timestamp {
        OBSERVED_AT.parse().unwrap()
    }

    #[test]
    fn process_identity_requires_exact_binary_argv_and_uid() {
        use local_api::process::{candidate_identity_matches, identity_matches};

        assert!(identity_matches(
            Some(501),
            501,
            Path::new("/home/me/.local/bin/agy"),
            OsStr::new("/home/me/.local/bin/agy"),
        ));
        assert!(!identity_matches(
            Some(502),
            501,
            Path::new("/home/me/.local/bin/agy"),
            OsStr::new("agy"),
        ));
        assert!(!identity_matches(
            Some(501),
            501,
            Path::new("/usr/bin/sh"),
            OsStr::new("agy"),
        ));
        assert!(!identity_matches(
            Some(501),
            501,
            Path::new("/home/me/.local/bin/agy"),
            OsStr::new("wrapper"),
        ));

        let candidate = local_api::Candidate {
            pid: 42,
            uid: 501,
            start_token: "1000".to_owned(),
            endpoints: Vec::new(),
        };
        assert!(candidate_identity_matches(
            &candidate,
            Some(501),
            Path::new("/home/me/.local/bin/agy"),
            OsStr::new("agy"),
            Some("1000"),
        ));
        assert!(!candidate_identity_matches(
            &candidate,
            Some(501),
            Path::new("/home/me/.local/bin/agy"),
            OsStr::new("agy"),
            Some("1001"),
        ));
    }

    #[test]
    fn linux_socket_tables_intersect_owned_inodes_and_loopback_only() {
        let inodes = BTreeSet::from(["12345".to_owned(), "67890".to_owned()]);
        let tcp = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n\
   0: 0100007F:1F90 00000000:0000 0A 0:0 00:0 0 501 0 12345\n\
   1: 00000000:2382 00000000:0000 0A 0:0 00:0 0 501 0 67890\n\
   2: 0100007F:2383 00000000:0000 01 0:0 00:0 0 501 0 67890\n\
   3: 0100007F:2384 00000000:0000 0A 0:0 00:0 0 501 0 99999";
        assert_eq!(
            local_api::process::parse_proc_net(tcp, &inodes, false),
            vec![LoopbackEndpoint {
                address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 8080,
            }]
        );

        let tcp6 = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n\
   0: 00000000000000000000000001000000:18EB 00000000000000000000000000000000:0000 0A 0:0 00:0 0 501 0 67890";
        assert_eq!(
            local_api::process::parse_proc_net(tcp6, &inodes, true),
            vec![LoopbackEndpoint {
                address: IpAddr::V6(Ipv6Addr::LOCALHOST),
                port: 6379,
            }]
        );
        assert_eq!(
            local_api::process::socket_inode("socket:[12345]").as_deref(),
            Some("12345")
        );
        assert!(local_api::process::socket_inode("pipe:[12345]").is_none());
    }

    #[test]
    fn lsof_parser_and_urls_accept_only_typed_loopback_endpoints() {
        let endpoints = local_api::process::parse_lsof(
            "p42\nn127.0.0.1:5000\nn[::1]:5001\nn0.0.0.0:5002\nn10.0.0.2:5003\n",
        );
        assert_eq!(endpoints.len(), 2);
        assert_eq!(
            LoopbackEndpoint {
                address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 5000,
            }
            .url("/status"),
            "https://127.0.0.1:5000/status"
        );
        assert_eq!(
            LoopbackEndpoint {
                address: IpAddr::V6(Ipv6Addr::LOCALHOST),
                port: 5001,
            }
            .url("/status"),
            "https://[::1]:5001/status"
        );
        assert_eq!(local_api::MAX_RESPONSE_BYTES, 256 * 1024);
    }

    #[test]
    fn identity_accepts_success_codes_and_prefers_user_tier() {
        for code in [json!(0), json!("0"), json!("ok"), json!("success")] {
            let body = json!({
                "code": code,
                "userStatus": {
                    "email": " user@example.com ",
                    "userTier": { "name": " Google AI Ultra " },
                    "planStatus": { "planInfo": { "planName": "Pro" } }
                }
            });
            let account = local_api::wire::parse_identity(&body.to_string()).unwrap();
            assert_eq!(account.account_id.as_deref(), Some("user@example.com"));
            assert_eq!(account.plan.as_deref(), Some("Google AI Ultra"));
            assert_eq!(account.metered, Some(true));
        }
    }

    #[test]
    fn identity_uses_plan_fallbacks_and_rejects_unusable_responses() {
        let account = local_api::wire::parse_identity(
            &json!({
                "userStatus": {
                    "planStatus": { "planInfo": { "displayName": " Antigravity Pro " } }
                }
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(account.plan.as_deref(), Some("Antigravity Pro"));

        for body in [
            json!({"code": 16, "userStatus": {"email": "user@example.com"}}),
            json!({"code": "denied", "userStatus": {"email": "user@example.com"}}),
            json!({"userStatus": {"email": " ", "userTier": {"name": " "}}}),
            json!({"response": {}}),
        ] {
            assert!(local_api::wire::parse_identity(&body.to_string()).is_err());
        }
    }

    #[test]
    fn quota_envelopes_and_fraction_shapes_normalize_to_two_windows() {
        let groups = json!([{
            "buckets": [
                {
                    "bucketId": "gemini-5h",
                    "remainingFraction": 0.8,
                    "resetTime": FIVE_HOUR_RESET
                },
                {
                    "bucketId": "3p-5h",
                    "remaining": {"remainingFraction": 0.3},
                    "resetTime": "2026-06-15T11:00:00Z"
                },
                {
                    "bucketId": "gemini-weekly",
                    "remaining": {"case": "remainingFraction", "value": 0.4},
                    "resetTime": WEEK_RESET
                },
                {
                    "bucketId": "3p-weekly",
                    "remaining": {"remainingFraction": 0.6},
                    "resetTime": "2026-06-21T08:00:00Z"
                }
            ]
        }]);
        for body in [
            json!({"response": {"groups": groups.clone()}}),
            json!({"summary": {"groups": groups.clone()}}),
            json!({"groups": groups.clone()}),
        ] {
            let limits =
                local_api::wire::parse_rate_limits(&body.to_string(), observed_at()).unwrap();
            assert_eq!(limits.windows.len(), 2);
            assert_eq!(limits.windows[0].duration_mins, Some(300));
            assert_eq!(limits.windows[0].used_percentage, Some(70));
            assert_eq!(limits.windows[1].duration_mins, Some(10_080));
            assert_eq!(limits.windows[1].used_percentage, Some(60));
            assert!(limits.windows.iter().all(|window| {
                window.source == crate::agents::context::WindowSource::Authoritative
                    && !window.lifted
            }));
        }
    }

    #[test]
    fn quota_reduction_breaks_ties_by_later_reset_then_identity() {
        let body = json!({"groups": [{"buckets": [
            {
                "bucketId": "z-5h",
                "remainingFraction": 0.25,
                "resetTime": "2026-06-15T10:00:00Z"
            },
            {
                "bucketId": "b-5h",
                "remainingFraction": 0.25,
                "resetTime": "2026-06-15T12:00:00Z"
            },
            {
                "bucketId": "a-5h",
                "remainingFraction": 0.25,
                "resetTime": "2026-06-15T12:00:00Z"
            }
        ]}]});
        let limits = local_api::wire::parse_rate_limits(&body.to_string(), observed_at()).unwrap();
        assert_eq!(limits.windows[0].used_percentage, Some(75));
        assert_eq!(
            limits.windows[0].resets_at,
            Some(FIVE_HOUR_RESET.parse().unwrap())
        );
    }

    #[test]
    fn quota_preserves_unknown_periods_and_accepts_one_period() {
        let body = json!({"groups": [{"buckets": [
            {"bucketId": "gemini-5h", "disabled": true, "remainingFraction": 0.5},
            {"bucketId": "3p-5h"},
            {
                "displayName": "Weekly Limit",
                "remainingFraction": 0.001,
                "resetTime": WEEK_RESET
            }
        ]}]});
        let limits = local_api::wire::parse_rate_limits(&body.to_string(), observed_at()).unwrap();
        assert_eq!(limits.windows[0].used_percentage, None);
        assert_eq!(limits.windows[0].resets_at, None);
        assert_eq!(limits.windows[1].used_percentage, Some(99));

        let exhausted = json!({"groups": [{"buckets": [{
            "bucketId": "gemini-weekly",
            "remainingFraction": 0,
            "resetTime": WEEK_RESET
        }]}]});
        let limits =
            local_api::wire::parse_rate_limits(&exhausted.to_string(), observed_at()).unwrap();
        assert_eq!(limits.windows.len(), 1);
        assert_eq!(limits.windows[0].used_percentage, Some(100));
    }

    #[test]
    fn quota_rejects_malformed_recognized_usage_and_unknown_only_payloads() {
        for bucket in [
            json!({"bucketId": "gemini-5h", "remainingFraction": -0.1, "resetTime": FIVE_HOUR_RESET}),
            json!({"bucketId": "gemini-5h", "remainingFraction": 1.1, "resetTime": FIVE_HOUR_RESET}),
            json!({"bucketId": "gemini-5h", "remainingFraction": 0.5, "resetTime": "invalid"}),
            json!({"bucketId": "gemini-5h", "remainingFraction": 0.5, "resetTime": OBSERVED_AT}),
        ] {
            let body = json!({"groups": [{"buckets": [
                bucket,
                {"bucketId": "3p-5h", "remainingFraction": 0.9, "resetTime": FIVE_HOUR_RESET}
            ]}]});
            assert!(local_api::wire::parse_rate_limits(&body.to_string(), observed_at()).is_err());
        }

        let unknown = json!({"groups": [{"buckets": [{
            "bucketId": "legacy-model-quota",
            "remainingFraction": 0.5,
            "resetTime": FIVE_HOUR_RESET
        }]}]});
        assert!(local_api::wire::parse_rate_limits(&unknown.to_string(), observed_at()).is_err());
    }

    #[test]
    fn adapter_keeps_dollars_credits_and_oauth_unsupported() {
        assert!(!AntigravityAdapter.descriptor().capabilities.account_spend);
        assert!(matches!(
            AntigravityAdapter.probe_oauth_usage(),
            crate::agents::OauthUsageProbe::Unsupported
        ));
        assert_eq!(AntigravityAdapter.oauth_credentials_stamp(), None);
        assert_eq!(AntigravityAdapter.oauth_account_key(), None);
        assert_eq!(AntigravityAdapter.probe_version(), None);
    }
}

fn write_transcript(home: &Path, session_id: &str) -> std::path::PathBuf {
    write_transcript_named(
        home,
        session_id,
        "transcript_full.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    )
}

fn write_transcript_named(
    home: &Path,
    session_id: &str,
    basename: &str,
    contents: &str,
) -> std::path::PathBuf {
    let path = home
        .join("brain")
        .join(session_id)
        .join(".system_generated/logs")
        .join(basename);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, contents).unwrap();
    path
}

fn user_record(step: u64, at: &str, content: &str) -> String {
    json!({
        "step_index": step,
        "source": "USER_EXPLICIT",
        "type": "USER_INPUT",
        "status": "DONE",
        "created_at": at,
        "content": content,
    })
    .to_string()
}

fn planner_record(step: u64, at: &str, tool_calls: Option<Value>) -> String {
    let mut record = json!({
        "step_index": step,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "status": "DONE",
        "created_at": at,
        "content": "planner response",
    });
    if let Some(tool_calls) = tool_calls {
        record["tool_calls"] = tool_calls;
    }
    record.to_string()
}

fn with<const N: usize>(base: &Value, fields: [(&str, Value); N]) -> Value {
    let mut value = base.clone();
    let object = value.as_object_mut().unwrap();
    for (key, field) in fields {
        object.insert(key.to_owned(), field);
    }
    value
}
