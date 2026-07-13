use super::*;

use crate::agents::AgentErr;
use serde_json::json;

#[test]
fn opencode_activity_filter_and_launch_commands_build() {
    let descriptor = OpencodeAdapter.descriptor();
    assert!(descriptor.records_activity("tool_after"));
    assert!(descriptor.records_activity("session_idle"));
    assert!(descriptor.records_activity("session_error"));
    assert!(descriptor.records_activity("SubagentStart"));
    assert!(!descriptor.records_activity("permission_ask"));
    assert!(!descriptor.records_activity("question_ask"));
    assert!(!descriptor.records_activity("session_compacting"));

    assert_eq!(
        OpencodeAdapter.resume_command("ses_123", Path::new("/tmp")),
        Some(vec![
            "opencode".to_owned(),
            "--session".to_owned(),
            "ses_123".to_owned(),
        ])
    );
    assert_eq!(
        OpencodeAdapter.fork_command("ses_123", Path::new("/tmp")),
        Some(vec![
            "opencode".to_owned(),
            "--session".to_owned(),
            "ses_123".to_owned(),
            "--fork".to_owned(),
        ])
    );
    assert_eq!(
        OpencodeAdapter.launch_command(&[], None),
        Some(vec!["opencode".to_owned()])
    );
    assert_eq!(
        OpencodeAdapter.launch_command(&["--pure".to_owned()], Some("review this")),
        Some(vec![
            "opencode".to_owned(),
            "--pure".to_owned(),
            "--".to_owned(),
            "review this".to_owned(),
        ])
    );
}

#[test]
fn opencode_render_preset_maps_model_and_rejects_unsupported_fields() {
    use crate::agents::{LaunchPreset, PresetErr};

    assert_eq!(
        OpencodeAdapter.render_preset(&LaunchPreset {
            model: Some("openai/gpt-5".to_owned()),
            ..Default::default()
        }),
        Ok(vec!["--model".to_owned(), "openai/gpt-5".to_owned()])
    );
    assert_eq!(
        OpencodeAdapter.render_preset(&LaunchPreset {
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "opencode",
            field: "effort",
        })
    );
    assert_eq!(
        OpencodeAdapter.render_preset(&LaunchPreset {
            system_prompt_file: Some(Path::new("/abs/prompt.md").to_path_buf()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "opencode",
            field: "system-prompt-file",
        })
    );
    assert_eq!(
        OpencodeAdapter.render_preset(&LaunchPreset {
            append_system_prompt_file: Some(Path::new("/abs/append.md").to_path_buf()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "opencode",
            field: "append-system-prompt-file",
        })
    );
    assert!(
        OpencodeAdapter
            .render_preset(&LaunchPreset::default())
            .expect("empty preset is valid")
            .is_empty()
    );
}

#[test]
fn opencode_observes_lifecycle_enrichment_and_boundaries() {
    let registered = OpencodeAdapter
        .observe_lifecycle(
            "session_created",
            &json!({
                "session_id": "ses_1",
                "cwd": "/home/u/repo",
                "model": "claude-sonnet-4.5",
                "effort": "xhigh",
                "input_tokens": 100,
                "cache_write_input_tokens": 40,
                "cache_read_input_tokens": 30,
                "output_tokens": 20,
                "total_tokens": 190
            }),
        )
        .expect("observation");
    assert_eq!(registered.agent_id.as_deref(), Some("ses_1"));
    assert_eq!(registered.signal, LifecycleSignal::Registered);
    assert_eq!(registered.worktree_path.as_deref(), Some("/home/u/repo"));
    assert_eq!(
        registered.launch.model.as_deref(),
        Some("claude-sonnet-4.5")
    );
    assert_eq!(registered.launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(registered.context_window, Some(200_000));
    assert_eq!(registered.fresh_input_tokens, Some(100));
    assert_eq!(registered.cache_write_input_tokens, Some(40));
    assert_eq!(registered.cache_read_input_tokens, Some(30));
    assert_eq!(registered.output_tokens, Some(20));
    assert_eq!(registered.total_tokens, Some(190));

    // A non-Claude session has no local fallback window; the plugin resolves it
    // from the model catalog — the model's max input tokens (`Model.limit.input`,
    // 272k for gpt-5.5, not the 400k total) — and stamps `context_window` on the
    // envelope, so the wire-carried value is used verbatim.
    let catalog_window = OpencodeAdapter
        .observe_lifecycle(
            "chat_message",
            &json!({
                "session_id": "ses_2",
                "model": "gpt-5.5",
                "provider_id": "openai",
                "context_window": 272_000
            }),
        )
        .expect("observation");
    assert_eq!(catalog_window.context_window, Some(272_000));
    // Without a stamped window, a non-Claude model stays unknown (Claude-only fallback).
    let unknown_window = OpencodeAdapter
        .observe_lifecycle(
            "chat_message",
            &json!({ "session_id": "ses_2", "model": "gpt-5.5", "provider_id": "openai" }),
        )
        .expect("observation");
    assert_eq!(unknown_window.context_window, None);

    let prompt = OpencodeAdapter
        .observe_lifecycle(
            "chat_message",
            &json!({ "session_id": "ses_1", "prompt": "  fix auth  " }),
        )
        .expect("observation");
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    assert_eq!(prompt.task.as_deref(), Some("fix auth"));

    let injected = OpencodeAdapter
        .observe_lifecycle(
            "chat_message",
            &json!({ "session_id": "ses_1", "prompt": "<system-reminder>noise" }),
        )
        .expect("observation");
    assert_eq!(injected.prompt, None);
    assert_eq!(injected.task, None);

    let idle = OpencodeAdapter
        .observe_lifecycle("session_idle", &json!({ "session_id": "ses_1" }))
        .expect("observation");
    assert_eq!(
        idle.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
    let error = OpencodeAdapter
        .observe_lifecycle(
            "session_error",
            &json!({ "session_id": "ses_1", "error_message": "boom" }),
        )
        .expect("observation");
    assert_eq!(
        error.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }
    );

    assert!(!OpencodeAdapter.ends_session("session_error"));
    let ended = OpencodeAdapter
        .observe_lifecycle(
            "session_ended",
            &json!({ "session_id": "ses_1", "reason": "deleted" }),
        )
        .expect("end observation");
    assert_eq!(ended.signal, LifecycleSignal::Ended);
    assert!(OpencodeAdapter.ends_session("session_ended"));
    assert!(OpencodeAdapter.moves_on("chat_message"));
    assert!(OpencodeAdapter.moves_on("session_idle"));
    assert!(OpencodeAdapter.moves_on("session_error"));
}

#[test]
fn opencode_observes_native_questions() {
    let observed = OpencodeAdapter
        .observe_lifecycle(
            "question_ask",
            &json!({ "session_id": "ses_1", "title": "Which database?" }),
        )
        .expect("question observation");
    assert_eq!(
        observed.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: Some("Which database?".to_owned()),
        }
    );
}

#[test]
fn opencode_parses_structured_native_questions() {
    let questions = OpencodeAdapter
        .ask_question_detail(
            "question_ask",
            &json!({
                "questions": [
                    {
                        "question": " Which database? ",
                        "header": "Database",
                        "options": [
                            { "label": " Postgres ", "description": "Relational" },
                            { "label": "", "description": "ignored" }
                        ],
                        "multiple": true,
                        "custom": true
                    },
                    { "question": "  ", "options": [] }
                ]
            }),
        )
        .expect("structured questions");
    assert_eq!(
        questions,
        vec![AskQuestion {
            question: "Which database?".to_owned(),
            options: vec![AskOption {
                label: "Postgres".to_owned(),
                description: Some("Relational".to_owned()),
                caution: None,
            }],
            multi_select: true,
            has_option_previews: false,
        }]
    );
    assert_eq!(
        OpencodeAdapter.ask_detail(
            "question_ask",
            &json!({ "questions": [{ "question": "Which database?\nPick one" }] }),
        ),
        Some("Which database?".to_owned())
    );
    assert!(
        OpencodeAdapter
            .ask_question_detail("question_ask", &json!({ "questions": "malformed" }))
            .is_none()
    );
    assert!(
        OpencodeAdapter
            .ask_question_detail("permission_ask", &json!({ "questions": [] }))
            .is_none()
    );
}

#[test]
fn opencode_records_native_permission_and_question_answers() {
    assert_eq!(
        OpencodeAdapter.native_ask_answer(
            "permission_replied",
            &json!({ "session_id": "ses_1", "reply": "always" }),
        ),
        Some(vec![AskAnswer {
            question: None,
            chosen: vec!["always".to_owned()],
            note: None,
        }])
    );
    assert_eq!(
        OpencodeAdapter.native_ask_answer(
            "question_replied",
            &json!({
                "session_id": "ses_1",
                "answers": [["Postgres"], ["Fast", "Safe"]]
            }),
        ),
        Some(vec![
            AskAnswer {
                question: None,
                chosen: vec!["Postgres".to_owned()],
                note: None,
            },
            AskAnswer {
                question: None,
                chosen: vec!["Fast".to_owned(), "Safe".to_owned()],
                note: None,
            },
        ])
    );
    assert_eq!(
        OpencodeAdapter.native_ask_answer("question_rejected", &json!({ "session_id": "ses_1" }),),
        Some(vec![AskAnswer {
            question: None,
            chosen: vec!["(rejected)".to_owned()],
            note: None,
        }])
    );
    assert!(
        OpencodeAdapter
            .native_ask_answer("question_replied", &json!({ "answers": [] }))
            .is_none()
    );
}

#[test]
fn opencode_context_refreshes_are_bounded_to_turn_events_with_server_url() {
    let ctx = crate::agents::LifecycleRefreshCtx {
        agent_id: "sess-1",
        workspace_id: "ws-1",
        model_hint: Some("gpt-5"),
        server_url: Some("http://127.0.0.1:4096/"),
    };
    for event in [
        "session_created",
        "chat_message",
        "session_idle",
        "session_error",
    ] {
        let spawn = OpencodeAdapter
            .context_refresh_spawn(crate::agents::RefreshTrigger::Hook(event), &ctx)
            .unwrap_or_else(|| panic!("{event} refreshes"));
        assert_eq!(
            spawn.args,
            [
                "opencode",
                "refresh-context",
                "--session-id",
                "sess-1",
                "--workspace-id",
                "ws-1",
                "--server-url",
                "http://127.0.0.1:4096/",
                "--model",
                "gpt-5",
            ],
            "{event}"
        );
    }

    let bare = crate::agents::LifecycleRefreshCtx {
        agent_id: "sess-1",
        workspace_id: "ws-1",
        model_hint: None,
        server_url: Some("http://127.0.0.1:4096/"),
    };
    assert!(
        !OpencodeAdapter
            .context_refresh_spawn(crate::agents::RefreshTrigger::Hook("session_idle"), &bare)
            .unwrap()
            .args
            .iter()
            .any(|arg| arg == "--model")
    );
    let missing_url = crate::agents::LifecycleRefreshCtx {
        agent_id: "sess-1",
        workspace_id: "ws-1",
        model_hint: None,
        server_url: None,
    };
    assert!(
        OpencodeAdapter
            .context_refresh_spawn(
                crate::agents::RefreshTrigger::Hook("session_idle"),
                &missing_url
            )
            .is_none()
    );
    assert!(
        OpencodeAdapter
            .context_refresh_spawn(crate::agents::RefreshTrigger::Tick, &ctx)
            .is_none()
    );
    for event in ["tool_after", "session_compacting", "session_compacted"] {
        assert!(
            OpencodeAdapter
                .context_refresh_spawn(crate::agents::RefreshTrigger::Hook(event), &ctx)
                .is_none(),
            "{event}"
        );
    }
}

#[test]
fn opencode_tool_compaction_subagent_and_unknown_events_map_cleanly() {
    for (tool_name, expected) in [
        (
            "edit",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
            }),
        ),
        (
            "bash",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
            }),
        ),
        ("read", None),
    ] {
        let observed = OpencodeAdapter.observe_lifecycle(
            "tool_after",
            &json!({ "session_id": "ses_1", "tool_name": tool_name }),
        );
        assert_eq!(observed.map(|obs| obs.signal), expected, "{tool_name}");
    }

    for event in [
        "permission_replied",
        "question_replied",
        "question_rejected",
    ] {
        let observed = OpencodeAdapter
            .observe_lifecycle(event, &json!({ "session_id": "ses_1" }))
            .unwrap_or_else(|| panic!("{event} observation"));
        assert_eq!(
            observed.signal,
            LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
            },
            "{event}"
        );
    }

    let compacting = OpencodeAdapter
        .observe_lifecycle("session_compacting", &json!({ "session_id": "ses_1" }))
        .expect("observation");
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    let compacted = OpencodeAdapter
        .observe_lifecycle("session_compacted", &json!({ "session_id": "ses_1" }))
        .expect("observation");
    assert_eq!(
        compacted.signal,
        LifecycleSignal::CompactionEnded { auto: None }
    );

    let child = OpencodeAdapter
        .observe_lifecycle(
            "SubagentStart",
            &json!({
                "session_id": "ses_child",
                "parent_session_id": "ses_parent",
                "prompt": "review auth"
            }),
        )
        .expect("observation");
    assert_eq!(child.agent_id.as_deref(), Some("ses_child"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("ses_parent"));
    assert_eq!(child.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(child.task.as_deref(), Some("review auth"));

    let child_stopped = OpencodeAdapter
        .observe_lifecycle(
            "SubagentStop",
            &json!({
                "session_id": "ses_child",
                "parent_session_id": "ses_parent",
                "is_error": true
            }),
        )
        .expect("observation");
    assert_eq!(
        child_stopped.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert_eq!(
        OpencodeAdapter.observe_lifecycle(
            "SubagentStart",
            &json!({ "session_id": "same", "parent_session_id": "same" }),
        ),
        None
    );
    assert_eq!(OpencodeAdapter.observe_lifecycle("bogus", &json!({})), None);
}

#[test]
fn neutral_decision_shape_is_pinned() {
    let rendered = OpencodeAdapter.render_neutral("permission_ask").unwrap();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}

#[test]
fn install_preview_and_uninstall_only_own_managed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin").join("rimz.ts");

    let report = OPENCODE_MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(report.agent, "opencode");
    assert!(!report.merged);
    assert_eq!(report.installed_events, managed_event_names());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);
    assert!(OPENCODE_MANAGED_SOURCE.installed_at(&path));

    std::fs::write(&path, "// still _rimz_managed\n// user tweak\n").unwrap();
    assert!(OPENCODE_MANAGED_SOURCE.install_into(&path).unwrap().merged);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);

    let preview = OPENCODE_MANAGED_SOURCE.preview_at(&path).unwrap();
    assert_eq!(preview.agent, "opencode");
    assert!(preview.merged);
    assert_eq!(preview.candidate_config, PLUGIN_SOURCE);

    let removed = OPENCODE_MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert!(removed.existed);
    assert_eq!(removed.removed_events, managed_event_names());
    assert!(!path.exists());
    assert!(!OPENCODE_MANAGED_SOURCE.installed_at(&path));
    assert!(
        !OPENCODE_MANAGED_SOURCE
            .uninstall_from(&path)
            .unwrap()
            .existed
    );

    let user_path = dir.path().join("user.ts");
    std::fs::write(&user_path, "// the user's own plugin\n").unwrap();
    assert!(matches!(
        OPENCODE_MANAGED_SOURCE
            .install_into(&user_path)
            .unwrap_err(),
        AgentErr::Install {
            agent: "opencode",
            ..
        }
    ));
    assert!(matches!(
        OPENCODE_MANAGED_SOURCE.preview_at(&user_path).unwrap_err(),
        AgentErr::Install {
            agent: "opencode",
            ..
        }
    ));
    let report = OPENCODE_MANAGED_SOURCE.uninstall_from(&user_path).unwrap();
    assert!(report.existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// the user's own plugin\n"
    );
    assert!(!OPENCODE_MANAGED_SOURCE.installed_at(&user_path));
}

fn managed_event_names() -> Vec<String> {
    WIRED_EVENTS
        .iter()
        .map(|event| (*event).to_owned())
        .collect()
}

#[test]
fn plugin_source_pins_rimz_wire_contract() {
    assert!(
        PLUGIN_SOURCE
            .lines()
            .next()
            .unwrap()
            .contains("_rimz_managed")
    );
    assert!(PLUGIN_SOURCE.contains("\"hooks\", \"feed\", \"--source\", \"opencode\""));
    assert!(PLUGIN_SOURCE.contains("RIMZ_AGENT_PID"));
    assert!(PLUGIN_SOURCE.contains("RIMZ_BIN"));
    assert!(PLUGIN_SOURCE.contains("server_url: input.serverUrl"));
    assert!(PLUGIN_SOURCE.contains("permission.ask"));
    assert!(PLUGIN_SOURCE.contains("permission.asked"));
    assert!(PLUGIN_SOURCE.contains("permission.replied"));
    assert!(PLUGIN_SOURCE.contains("question.asked"));
    assert!(PLUGIN_SOURCE.contains("question.replied"));
    assert!(PLUGIN_SOURCE.contains("question.rejected"));
    assert!(PLUGIN_SOURCE.contains("session.deleted"));
    assert!(PLUGIN_SOURCE.contains("Promise.allSettled"));
    assert!(PLUGIN_SOURCE.contains("endRoot(sessionID, \"dispose\")"));
    assert!(PLUGIN_SOURCE.contains("{\"status\":\"deny\"}"));
    assert!(PLUGIN_SOURCE.contains("export const RimzPlugin"));
    assert!(PLUGIN_SOURCE.contains("server: RimzPlugin"));
    // The gauge carries a catalog-resolved context window on every envelope,
    // and the divisor is the model's max input tokens (the uniform cross-agent
    // meaning), falling back to the total context only when no input cap exists.
    assert!(PLUGIN_SOURCE.contains("context_window: currentGauge?.contextWindow"));
    assert!(PLUGIN_SOURCE.contains("input.client.config.providers()"));
    assert!(PLUGIN_SOURCE.contains("limit?.input ?? limit?.context"));

    for event in WIRED_EVENTS {
        assert!(
            PLUGIN_SOURCE.contains(event),
            "plugin source missing {event}"
        );
    }
}
