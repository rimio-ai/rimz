use super::*;

use crate::agents::AgentErr;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output};
use crate::transcript::{AskOption, AskQuestion};
use serde_json::json;

#[test]
fn models_render_without_argv_and_restore_from_the_session() {
    let launch = OpencodeAdapter.spec().launch;
    assert_eq!(
        OpencodeAdapter
            .spec()
            .render_preset(&crate::agents::LaunchPreset {
                model: Some("opencode-go/deepseek-v4.1-flash".into()),
                ..Default::default()
            }),
        Ok(Vec::new())
    );
    assert!(launch.restores_session_model);
    assert!(
        launch
            .preset_arg_matcher(crate::agents::PresetField::Model)
            .is_none()
    );
}

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
            "--standalone".to_owned(),
        ])
    );
    assert_eq!(OpencodeAdapter.spec().launch.fork_command("ses_123"), None);
    assert_eq!(
        OpencodeAdapter.launch_command(&[], None),
        Some(vec!["opencode".to_owned(), "--standalone".to_owned()])
    );
    assert_eq!(
        OpencodeAdapter.launch_command(&["--pure".to_owned()], Some("review this")),
        Some(vec![
            "opencode".to_owned(),
            "--standalone".to_owned(),
            "--pure".to_owned(),
            "--prompt".to_owned(),
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
        Vec::<String>::new()
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
fn composed_system_prompt_preserves_config_with_or_without_delegation() {
    let raw = r#"{/* JSONC */"theme":"dark","plugins":"untouched",
        "agents":{"review":{"system":"review"},"build":{"model":"custom"}}}"#;
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for disable_delegation in [false, true] {
        let raw = if disable_delegation {
            raw.replace("\"untouched\"", "[\"custom\"]")
        } else {
            raw.to_owned()
        };
        let inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw)]);
        let mut env = BTreeMap::new();
        let warnings = OpencodeAdapter.finalize_launch_env(
            &inherited,
            &mut env,
            disable_delegation,
            Some(Path::new("/runtime/prompt.md")),
        );
        actual.push((
            env.get("OPENCODE_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warnings,
        ));
        let plugins = if disable_delegation {
            json!(["custom", "-opencode.tool.subagent"])
        } else {
            json!("untouched")
        };
        expected.push((
            Some(json!({"theme":"dark", "plugins":plugins,
            "agents":{"review":{"system":"review"},"build":{
                "model":"custom","system":"{file:/runtime/prompt.md}"
            }}})),
            Vec::<String>::new(),
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn composed_system_prompt_replaces_inherited_system_with_one_warning() {
    let mut actual = Vec::new();
    for system in [
        json!("previous"),
        json!(""),
        json!(null),
        json!({"file":"old"}),
    ] {
        let inherited = BTreeMap::from([(
            "OPENCODE_CONFIG_CONTENT".into(),
            json!({"agents":{"build":{"system":system}}}).to_string(),
        )]);
        let mut env = BTreeMap::new();
        let warnings = OpencodeAdapter.finalize_launch_env(
            &inherited,
            &mut env,
            false,
            Some(Path::new("/runtime/prompt.md")),
        );
        actual.push((
            env.get("OPENCODE_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warnings,
        ));
    }
    let warning = "OPENCODE_CONFIG_CONTENT set agents.build.system; RimZ's composed system prompt \
                   replaced it for this launch; move that prompt into agents/opencode.md or a \
                   profile's system-prompt-file, or remove it from the variable";
    assert_eq!(
        actual,
        vec![
            (
                Some(json!({"agents":{"build":{"system":"{file:/runtime/prompt.md}"}}})),
                vec![warning.to_owned()],
            );
            4
        ]
    );
}

#[test]
fn malformed_prompt_and_delegation_config_warns_once_and_keeps_both_edits() {
    let mut actual = Vec::new();
    for (raw, member, shape, problem) in [
        ("not JSON", "plugins", "an array", "is not valid JSONC"),
        ("[]", "plugins", "an array", "is not a JSON object"),
        (
            r#"{"plugins":false}"#,
            "plugins",
            "an array",
            "sets plugins to a value that is not an array",
        ),
        (
            r#"{"agents":[]}"#,
            "agents",
            "an object",
            "sets agents to a value that is not an object",
        ),
        (
            r#"{"agents":{"build":null}}"#,
            "agents.build",
            "an object",
            "sets agents.build to a value that is not an object",
        ),
    ] {
        let inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]);
        let mut env = BTreeMap::new();
        let warnings = OpencodeAdapter.finalize_launch_env(
            &inherited,
            &mut env,
            true,
            Some(Path::new("/runtime/prompt.md")),
        );
        let expected_warning = format!(
            "OPENCODE_CONFIG_CONTENT {problem}; this launch ignored the configured value; \
             set it to a JSON object whose {member} is {shape}, or unset it"
        );
        actual.push((
            env.get("OPENCODE_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warnings == [expected_warning],
        ));
    }
    assert_eq!(
        actual,
        vec![
            (
                Some(json!({"plugins":["-opencode.tool.subagent"],
            "agents":{"build":{"system":"{file:/runtime/prompt.md}"}}})),
                true
            );
            5
        ]
    );
}

#[test]
fn unmanaged_core_config_is_untouched_without_prompt_or_delegation() {
    let mut actual = Vec::new();
    for raw in [
        None,
        Some("not JSON"),
        Some(r#"{"agents":{"build":{"system":"keep"}}}"#),
    ] {
        let inherited = raw
            .map(|raw| BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]))
            .unwrap_or_default();
        let mut env = BTreeMap::new();
        let warnings = OpencodeAdapter.finalize_launch_env(&inherited, &mut env, false, None);
        actual.push((env.contains_key("OPENCODE_CONFIG_CONTENT"), warnings));
    }
    assert_eq!(actual, vec![(false, Vec::<String>::new()); 3]);
}

#[test]
fn native_subagent_denial_preserves_opencode_config_and_appends_plugin_selector() {
    let selector = "-opencode.tool.subagent";
    let allow = json!({ "action": "other", "resource": "review", "effect": "allow" });
    let raw = format!(
        "{{/* JSONC */\"theme\":\"dark\",\"permissions\":[{allow},],\"plugins\":[\"custom\",],}}"
    );
    let mut actual = Vec::new();
    for value in [
        None,
        Some(raw.as_str()),
        Some(r#"{"plugins":["custom","-opencode.tool.subagent"]}"#),
    ] {
        let inherited = value
            .map(|value| BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), value.into())]))
            .unwrap_or_default();
        let mut env = BTreeMap::new();
        let warning = OpencodeAdapter
            .finalize_launch_env(&inherited, &mut env, true, None)
            .into_iter()
            .next();
        actual.push((
            serde_json::from_str::<Value>(&env["OPENCODE_CONFIG_CONTENT"]).unwrap(),
            warning,
        ));
    }
    assert_eq!(
        actual,
        vec![
            (json!({"plugins":[selector]}), None),
            (
                json!({"theme":"dark", "permissions":[allow], "plugins":["custom",selector]}),
                None
            ),
            (json!({"plugins":["custom",selector]}), None),
        ]
    );
}

#[test]
fn native_subagent_denial_warns_when_opencode_config_is_ignored() {
    let mut actual = Vec::new();
    for (raw, problem) in [
        ("not JSON", "not valid JSONC"),
        ("[]", "not a JSON object"),
        (r#"{"plugins":{}}"#, "not an array"),
    ] {
        let inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]);
        let mut env = BTreeMap::new();
        let warning = OpencodeAdapter
            .finalize_launch_env(&inherited, &mut env, true, None)
            .into_iter()
            .next()
            .unwrap_or_default();
        actual.push((
            serde_json::from_str::<Value>(&env["OPENCODE_CONFIG_CONTENT"]).unwrap(),
            [
                "OPENCODE_CONFIG_CONTENT",
                problem,
                "ignored the configured value",
                "JSON object",
                "plugins",
                "array",
                "unset",
            ]
            .iter()
            .all(|part| warning.contains(part)),
        ));
    }
    assert_eq!(
        actual,
        vec![(json!({"plugins":["-opencode.tool.subagent"]}), true); 3]
    );
}

#[test]
fn managed_launch_hides_tabs_preserving_other_cli_config() {
    let mut actual = Vec::new();
    for raw in [
        None,
        Some(""),
        Some(
            r#"{/* JSONC */"theme":"dark","tabs":{
                "mode":"on","layout":"vertical","enabled":true,},}"#,
        ),
        Some(r#"{"tabs":{"mode":"auto"}}"#),
        Some(r#"{"theme":"light"}"#),
    ] {
        let mut inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), "keep".into())]);
        if let Some(raw) = raw {
            inherited.insert("OPENCODE_CLI_CONFIG_CONTENT".into(), raw.into());
        }
        let mut env = BTreeMap::new();
        let warning = OpencodeAdapter
            .finalize_launch_env(&inherited, &mut env, false, None)
            .into_iter()
            .next();
        actual.push((
            env.get("OPENCODE_CLI_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warning,
            env.contains_key("OPENCODE_CONFIG_CONTENT"),
        ));
    }
    assert_eq!(
        actual,
        vec![
            (Some(json!({"tabs":{"mode":"off"}})), None, false),
            (Some(json!({"tabs":{"mode":"off"}})), None, false),
            (
                Some(json!({"theme":"dark","tabs":{
                    "mode":"off","layout":"vertical","enabled":true
                }})),
                None,
                false,
            ),
            (Some(json!({"tabs":{"mode":"off"}})), None, false),
            (
                Some(json!({"theme":"light","tabs":{"mode":"off"}})),
                None,
                false
            ),
        ]
    );
}

#[test]
fn managed_launch_warns_when_opencode_cli_config_is_ignored() {
    let mut actual = Vec::new();
    for (raw, problem) in [
        ("not JSON", "not valid JSONC"),
        (" ", "not valid JSONC"),
        ("[]", "not a JSON object"),
        ("null", "not a JSON object"),
        ("42", "not a JSON object"),
        ("true", "not a JSON object"),
        (r#""config""#, "not a JSON object"),
        (r#"{"tabs":"off"}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":null}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":[]}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":42}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":false}"#, "tabs to a value that is not an object"),
    ] {
        let inherited = BTreeMap::from([("OPENCODE_CLI_CONFIG_CONTENT".into(), raw.into())]);
        let mut env = BTreeMap::new();
        let warning = OpencodeAdapter
            .finalize_launch_env(&inherited, &mut env, false, None)
            .into_iter()
            .next()
            .unwrap_or_default();
        actual.push((
            env.get("OPENCODE_CLI_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            [
                "OPENCODE_CLI_CONFIG_CONTENT",
                problem,
                "ignored the configured value",
                "JSON object",
                "tabs",
                "unset",
            ]
            .iter()
            .all(|part| warning.contains(part)),
        ));
    }
    assert_eq!(
        actual,
        vec![(Some(json!({"tabs":{"mode":"off"}})), true); 12]
    );
}

#[test]
#[cfg(unix)]
fn install_blocker_refuses_only_old_opencode() {
    use std::os::unix::fs::PermissionsExt as _;
    for (banner, blocked) in [
        ("opencode v1.19.0", true),
        ("opencode v2.0.27", false),
        ("unknown", false),
    ] {
        // The version probe memoizes per path and file stamp, so a same-length
        // rewrite of one path within an mtime tick would read the last case's output.
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("opencode");
        std::fs::write(&binary, format!("#!/bin/sh\nprintf '%s\\n' '{banner}'\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let reason = install_blocker_at(&binary);
        assert_eq!(reason.is_some(), blocked, "{banner}");
        if let Some(reason) = reason {
            assert_eq!(
                reason,
                format!(
                    "opencode 1.19.0 at {} is older than 2.0.0, the oldest release RimZ supports; run `opencode upgrade --major`",
                    binary.display()
                )
            );
        }
    }
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
    assert_eq!(
        prompt.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
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
            turn_id: None,
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
            turn_id: None,
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
                turn_id: None,
            }),
        ),
        (
            "bash",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                name: Some("bash".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "read",
            Some(LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: Some("read".to_owned()),
                native_key: None,
                turn_id: None,
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
                turn_id: None,
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
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: false,
        }
    );

    let failed = hook_lifecycle(
        &OpencodeAdapter,
        "session_compacted",
        &json!({ "session_id": "ses_1", "is_error": true }),
    );
    assert_eq!(
        failed.signal,
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: true,
        }
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
    assert!(PLUGIN_SOURCE.contains("stdio: [\"pipe\", \"ignore\", \"ignore\"]"));

    for hook in OPENCODE_HOOKS {
        let event = hook.event;
        assert!(
            PLUGIN_SOURCE.contains(event),
            "plugin source missing {event}"
        );
    }
}
