use super::*;

use crate::agents::AgentErr;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output};
use crate::transcript::{AskOption, AskQuestion};
use serde_json::json;

#[test]
fn opencode_activity_filter_and_launch_commands_build() {
    for event in [
        "tool_after",
        "session_idle",
        "session_error",
        "SubagentStart",
    ] {
        assert!(
            hook_output(&OpencodeAdapter, event, &json!({ "session_id": "ses_1" }))
                .records_progress(),
            "{event} records progress"
        );
    }
    for event in ["permission_ask", "question_ask", "session_compacting"] {
        assert!(
            !hook_output(&OpencodeAdapter, event, &json!({ "session_id": "ses_1" }))
                .records_progress(),
            "{event} does not record progress"
        );
    }

    assert_eq!(
        OpencodeAdapter.resume_command("ses_123", Path::new("/tmp")),
        Some(vec![
            "opencode".to_owned(),
            "--session".to_owned(),
            "ses_123".to_owned(),
        ])
    );
    assert_eq!(
        OpencodeAdapter.spec().launch.fork_command("ses_123"),
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
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Ask),
        Vec::<String>::new()
    );
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
        Vec::<String>::new()
    );
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Plan),
        vec!["--agent", "plan"]
    );
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Yolo),
        vec!["--auto"]
    );
}

#[test]
fn subagent_lockdown_merges_opencode_permissions() {
    let mut absent = std::collections::BTreeMap::new();
    OpencodeAdapter.lockdown_subagent_env(&mut absent);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&absent["OPENCODE_PERMISSION"])
            .expect("permission JSON"),
        json!({ "task": "deny" })
    );

    let mut existing = std::collections::BTreeMap::from([(
        "OPENCODE_PERMISSION".to_owned(),
        r#"{"bash":"ask","edit":"allow","task":"allow"}"#.to_owned(),
    )]);
    OpencodeAdapter.lockdown_subagent_env(&mut existing);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&existing["OPENCODE_PERMISSION"])
            .expect("permission JSON"),
        json!({ "bash": "ask", "edit": "allow", "task": "deny" })
    );

    let mut invalid = std::collections::BTreeMap::from([(
        "OPENCODE_PERMISSION".to_owned(),
        "not JSON".to_owned(),
    )]);
    OpencodeAdapter.lockdown_subagent_env(&mut invalid);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&invalid["OPENCODE_PERMISSION"])
            .expect("permission JSON"),
        json!({ "task": "deny" })
    );
}

#[test]
fn opencode_observes_lifecycle_enrichment_and_boundaries() {
    let registered = hook_lifecycle(
        &OpencodeAdapter,
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
    );
    assert_eq!(registered.agent_id.as_deref(), Some("ses_1"));
    assert_eq!(registered.signal, LifecycleSignal::Registered);
    assert_eq!(registered.worktree_path.as_deref(), Some("/home/u/repo"));
    assert_eq!(
        registered.launch.model.as_deref(),
        Some("claude-sonnet-4.5")
    );
    assert_eq!(registered.launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(registered.usage.context_window, Some(200_000));
    assert_eq!(registered.usage.fresh_input_tokens, Some(100));
    assert_eq!(registered.usage.cache_write_input_tokens, Some(40));
    assert_eq!(registered.usage.cache_read_input_tokens, Some(30));
    assert_eq!(registered.usage.output_tokens, Some(20));
    assert_eq!(registered.usage.total_tokens, Some(190));

    // A non-Claude session has no local fallback window; the plugin resolves it
    // from the model catalog — the model's max input tokens (`Model.limit.input`,
    // 272k for gpt-5.5, not the 400k total) — and stamps `context_window` on the
    // envelope, so the wire-carried value is used verbatim.
    let catalog_window = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({
            "session_id": "ses_2",
            "model": "gpt-5.5",
            "provider_id": "openai",
            "context_window": 272_000
        }),
    );
    assert_eq!(catalog_window.usage.context_window, Some(272_000));
    // Without a stamped window, a non-Claude model stays unknown (Claude-only fallback).
    let unknown_window = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({ "session_id": "ses_2", "model": "gpt-5.5", "provider_id": "openai" }),
    );
    assert_eq!(unknown_window.usage.context_window, None);

    let prompt = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({ "session_id": "ses_1", "prompt": "  fix auth  " }),
    );
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    assert_eq!(prompt.task.as_deref(), Some("fix auth"));

    let injected = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({ "session_id": "ses_1", "prompt": "<system-reminder>noise" }),
    );
    assert_eq!(injected.prompt, None);
    assert_eq!(injected.task, None);

    let idle = hook_lifecycle(
        &OpencodeAdapter,
        "session_idle",
        &json!({ "session_id": "ses_1" }),
    );
    assert_eq!(
        idle.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
    let proposed_plan = hook_lifecycle(
        &OpencodeAdapter,
        "session_idle",
        &json!({ "session_id": "ses_1", "plan_proposed": true }),
    );
    assert_eq!(
        proposed_plan.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::PlanApproval,
            ask_id: None,
            detail: None,
            native_key: None,
        }
    );
    let error = hook_lifecycle(
        &OpencodeAdapter,
        "session_error",
        &json!({ "session_id": "ses_1", "error_message": "boom" }),
    );
    assert_eq!(
        error.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }
    );

    let ended_decoded = hook_output(
        &OpencodeAdapter,
        "session_ended",
        &json!({ "session_id": "ses_1", "reason": "deleted" }),
    );
    let ended = ended_decoded.lifecycle().cloned().expect("end observation");
    assert_eq!(ended.signal, LifecycleSignal::Ended);
    assert!(ended_decoded.ends_session());
    assert!(
        !hook_output(
            &OpencodeAdapter,
            "session_error",
            &json!({ "session_id": "ses_1" })
        )
        .ends_session()
    );
}

#[test]
fn opencode_observes_rich_context_from_plugin_envelopes() {
    let output = hook_output(
        &OpencodeAdapter,
        "session_updated",
        &json!({
            "session_id": "ses_1",
            "session_name": "Fix OpenCode metadata",
            "model_display_name": "GPT-5.5 Codex",
            "agent_version": "1.18.23"
        }),
    );
    let observation = output.observed_context().expect("rich context");
    assert_eq!(observation.agent_id.as_str(), "ses_1");
    assert_eq!(observation.context.source, "opencode");
    assert_eq!(
        observation.context.session_name.as_deref(),
        Some("Fix OpenCode metadata")
    );
    assert_eq!(
        observation.context.model_display_name.as_deref(),
        Some("GPT-5.5 Codex")
    );
    assert_eq!(
        observation.context.agent_version.as_deref(),
        Some("1.18.23")
    );
    assert!(output.lifecycle().is_none());

    assert!(
        hook_output(
            &OpencodeAdapter,
            "session_idle",
            &json!({ "session_id": "ses_1" })
        )
        .observed_context()
        .is_none()
    );
}

#[test]
fn opencode_observes_native_questions() {
    let observed = hook_lifecycle(
        &OpencodeAdapter,
        "question_ask",
        &json!({ "session_id": "ses_1", "title": "Which database?" }),
    );
    assert_eq!(
        observed.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: Some("Which database?".to_owned()),
            native_key: None,
        }
    );
}

#[test]
fn opencode_parses_structured_native_questions() {
    let questions = hook_output(
        &OpencodeAdapter,
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
    .questions()
    .to_vec();
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
        hook_output(
            &OpencodeAdapter,
            "question_ask",
            &json!({ "questions": [{ "question": "Which database?\nPick one" }] })
        )
        .ask_detail()
        .map(str::to_owned),
        Some("Which database?".to_owned())
    );
    assert!(
        hook_output(
            &OpencodeAdapter,
            "question_ask",
            &json!({ "questions": "malformed" })
        )
        .questions()
        .to_vec()
        .is_empty()
    );
    assert!(
        hook_output(
            &OpencodeAdapter,
            "permission_ask",
            &json!({ "questions": [] })
        )
        .questions()
        .to_vec()
        .is_empty()
    );
}

#[test]
fn opencode_caps_normalized_questions_after_filtering() {
    let questions = hook_output(
        &OpencodeAdapter,
        "question_ask",
        &json!({
            "questions": [
                {"question": "one"},
                {"question": "   "},
                {"question": "two"},
                {"question": "three"},
                {"question": "four"},
                {"question": "five"}
            ]
        }),
    )
    .questions()
    .iter()
    .map(|question| question.question.clone())
    .collect::<Vec<_>>();
    assert_eq!(questions, ["one", "two", "three", "four"]);
}

#[test]
fn opencode_records_native_permission_and_question_answers() {
    assert_eq!(
        hook_output(
            &OpencodeAdapter,
            "permission_replied",
            &json!({ "session_id": "ses_1", "reply": "always" })
        )
        .native_answers()
        .map(<[_]>::to_vec),
        Some(vec![AskAnswer {
            question: None,
            chosen: vec!["always".to_owned()],
            note: None,
        }])
    );
    assert_eq!(
        hook_output(
            &OpencodeAdapter,
            "question_replied",
            &json!({
                "session_id": "ses_1",
                "answers": [["Postgres"], ["Fast", "Safe"]]
            })
        )
        .native_answers()
        .map(<[_]>::to_vec),
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
        hook_output(
            &OpencodeAdapter,
            "question_rejected",
            &json!({ "session_id": "ses_1" })
        )
        .native_answers()
        .map(<[_]>::to_vec),
        Some(vec![AskAnswer {
            question: None,
            chosen: vec!["(rejected)".to_owned()],
            note: None,
        }])
    );
    assert!(
        hook_output(
            &OpencodeAdapter,
            "question_replied",
            &json!({ "answers": [] })
        )
        .native_answers()
        .map(<[_]>::to_vec)
        .is_none()
    );
}

#[test]
fn opencode_tool_compaction_subagent_and_unknown_events_map_cleanly() {
    for (tool_name, expected) in [
        (
            "edit",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
                name: Some("edit".to_owned()),
                native_key: None,
            }),
        ),
        (
            "bash",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                name: Some("bash".to_owned()),
                native_key: None,
            }),
        ),
        (
            "read",
            Some(LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: Some("read".to_owned()),
                native_key: None,
            }),
        ),
    ] {
        let observed = hook_observation(
            &OpencodeAdapter,
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
        let observed = hook_observation(&OpencodeAdapter, event, &json!({ "session_id": "ses_1" }))
            .unwrap_or_else(|| panic!("{event} observation"));
        assert_eq!(
            observed.signal,
            LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: None,
                native_key: None,
            },
            "{event}"
        );
    }

    let compacting = hook_lifecycle(
        &OpencodeAdapter,
        "session_compacting",
        &json!({ "session_id": "ses_1" }),
    );
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    let compacted = hook_lifecycle(
        &OpencodeAdapter,
        "session_compacted",
        &json!({ "session_id": "ses_1" }),
    );
    assert_eq!(
        compacted.signal,
        LifecycleSignal::CompactionEnded { auto: None }
    );

    let child = hook_lifecycle(
        &OpencodeAdapter,
        "SubagentStart",
        &json!({
            "session_id": "ses_child",
            "parent_session_id": "ses_parent",
            "prompt": "review auth",
            "model": "claude-sonnet-4-5"
        }),
    );
    assert_eq!(child.agent_id.as_deref(), Some("ses_child"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("ses_parent"));
    assert_eq!(child.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(child.task.as_deref(), Some("review auth"));
    assert_eq!(child.launch.model.as_deref(), Some("claude-sonnet-4-5"));

    let child_stopped = hook_lifecycle(
        &OpencodeAdapter,
        "SubagentStop",
        &json!({
            "session_id": "ses_child",
            "parent_session_id": "ses_parent",
            "is_error": true
        }),
    );
    assert_eq!(
        child_stopped.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert_eq!(
        hook_observation(
            &OpencodeAdapter,
            "SubagentStart",
            &json!({ "session_id": "same", "parent_session_id": "same" })
        ),
        None
    );
    assert_eq!(
        hook_observation(&OpencodeAdapter, "bogus", &json!({})),
        None
    );
}

#[test]
fn neutral_decision_shape_is_pinned() {
    let rendered = hook_output(&OpencodeAdapter, "permission_ask", &Value::Null)
        .json_reply()
        .cloned();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}

#[test]
fn install_preview_and_uninstall_only_own_managed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin").join("rimz.ts");

    let report = OPENCODE_MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(report.agent, "opencode");
    assert!(!report.files[0].existed);
    assert_eq!(report.installed_events, managed_event_names());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);
    assert!(OPENCODE_MANAGED_SOURCE.installed_at(&path));
    assert!(!OPENCODE_MANAGED_SOURCE.upgrade_available_at(&path));

    let stale = "// still _rimz_managed\n// older RimZ source\n";
    std::fs::write(&path, stale).unwrap();
    assert!(OPENCODE_MANAGED_SOURCE.installed_at(&path));
    assert!(OPENCODE_MANAGED_SOURCE.upgrade_available_at(&path));
    assert!(OPENCODE_MANAGED_SOURCE.install_into(&path).unwrap().files[0].existed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);
    assert!(!OPENCODE_MANAGED_SOURCE.upgrade_available_at(&path));

    let preview = OPENCODE_MANAGED_SOURCE.preview_at(&path).unwrap();
    assert_eq!(preview.agent, "opencode");
    assert!(preview.files[0].existed);
    assert_eq!(preview.files[0].candidate, PLUGIN_SOURCE);

    let removed = OPENCODE_MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert!(removed.files[0].existed);
    assert_eq!(removed.removed_events, managed_event_names());
    assert!(!path.exists());
    assert!(!OPENCODE_MANAGED_SOURCE.installed_at(&path));
    assert!(!OPENCODE_MANAGED_SOURCE.upgrade_available_at(&path));
    assert!(!OPENCODE_MANAGED_SOURCE.uninstall_from(&path).unwrap().files[0].existed);

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
    assert!(report.files[0].existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// the user's own plugin\n"
    );
    assert!(!OPENCODE_MANAGED_SOURCE.installed_at(&user_path));
    assert!(!OPENCODE_MANAGED_SOURCE.upgrade_available_at(&user_path));
}

fn managed_event_names() -> Vec<String> {
    OPENCODE_HOOKS
        .iter()
        .map(|hook| hook.event.to_owned())
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
    assert!(PLUGIN_SOURCE.contains("session_name: sessionID ? sessions.get(sessionID)?.title"));
    assert!(PLUGIN_SOURCE.contains("agent_version: sessionID ? sessions.get(sessionID)?.version"));
    assert!(PLUGIN_SOURCE.contains("model_display_name: currentGauge?.modelDisplayName"));
    assert!(!PLUGIN_SOURCE.contains("server_url: input.serverUrl"));
    assert!(PLUGIN_SOURCE.contains("permission.ask"));
    assert!(PLUGIN_SOURCE.contains("permission.asked"));
    assert!(PLUGIN_SOURCE.contains("permission.replied"));
    assert!(PLUGIN_SOURCE.contains("question.asked"));
    assert!(PLUGIN_SOURCE.contains("question.replied"));
    assert!(PLUGIN_SOURCE.contains("question.rejected"));
    assert!(PLUGIN_SOURCE.contains("session.deleted"));
    assert!(PLUGIN_SOURCE.contains("session.updated"));
    assert!(PLUGIN_SOURCE.contains("DEFAULT_SESSION_TITLE"));
    assert!(PLUGIN_SOURCE.contains("plan_proposed"));
    assert!(PLUGIN_SOURCE.contains("agents.get(sessionID) === \"plan\""));
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
    assert!(PLUGIN_SOURCE.contains("const hasMeasuredUsage ="));
    assert!(PLUGIN_SOURCE.contains("(value) => (value ?? 0) > 0"));

    for hook in OPENCODE_HOOKS {
        let event = hook.event;
        assert!(
            PLUGIN_SOURCE.contains(event),
            "plugin source missing {event}"
        );
    }
}
