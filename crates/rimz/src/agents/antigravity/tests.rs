use super::*;
use crate::agents::{
    AgentStatus, LaunchPreset, LocalSessionObservation, LocalSessionProjection, LocalSessionState,
    PriceBook, StatusLineChange, TranscriptRole, TurnPhase,
};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};
const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const CHILD_ALPHA: &str = "15b124d3-7753-412b-988b-88b2cd518cf8";
const CHILD_BETA: &str = "c86b1a56-5c62-43b4-9055-102461a074ef";
const NESTED_CHILD: &str = "44444444-4444-4444-8444-444444444444";
const AT_09: &str = "2026-07-13T23:23:09Z";
const AT_10: &str = "2026-07-13T23:23:10Z";
const AT_11: &str = "2026-07-13T23:23:11Z";
const AT_12: &str = "2026-07-13T23:23:12Z";
const FLASH: &str = "Gemini 3.5 Flash";
const PRO: &str = "Gemini 3.1 Pro";
const SONNET: &str = "Claude Sonnet 4.6";
const TURBO: &str = "Gemini 3.5 Flash (Turbo)";
const MALFORMED_MODEL: &str = "Gemini 3.5 Flash (Low";
fn local_state(observation: &LocalSessionObservation) -> &LocalSessionState {
    let LocalSessionProjection::Lifecycle(state) = &observation.projection else {
        panic!("Antigravity discovery must project lifecycle")
    };
    state
}
fn hook_payload() -> Value {
    json_value(
        r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","modelName":"Gemini 3.5 Flash"}"#,
    )
}
#[test]
fn observer_neutral_hooks_leave_pre_tool_policy_untouched() {
    let payload = hook_payload();
    assert_eq!(
        AntigravityAdapter
            .decode_hook("PreInvocation", &Value::Null)
            .expect("test hook decodes")
            .neutral,
        Some(json!({}))
    );
    assert_eq!(
        AntigravityAdapter
            .decode_hook("Stop", &Value::Null)
            .expect("test hook decodes")
            .neutral,
        Some(json!({"decision": ""}))
    );
    assert_eq!(
        AntigravityAdapter
            .decode_hook("PreToolUse", &payload)
            .expect("test hook decodes")
            .class,
        AgentHookClass::Unknown
    );
    assert_eq!(
        AntigravityAdapter
            .decode_hook("PreToolUse", &Value::Null)
            .expect("test hook decodes")
            .neutral,
        None
    );
}
#[test]
fn native_hooks_normalize_lifecycle() {
    let common = hook_payload();
    let tool = |mutates, edits| LifecycleSignal::ToolUsed {
        mutates,
        edits,
        native_key: None,
    };
    let ended = |errored, parked_on_background| LifecycleSignal::TurnEnded {
        errored,
        parked_on_background,
    };
    let started = AntigravityAdapter
        .decode_hook(
            "PreInvocation",
            &with(&common, [("invocationNum", json!(0))]),
        )
        .expect("test hook decodes")
        .lifecycle
        .unwrap();
    assert_eq!(started.signal, LifecycleSignal::TurnStarted);
    assert_eq!(started.agent_id.as_deref(), Some(SESSION_ID));
    assert_eq!(started.worktree_path.as_deref(), Some("/workspace/project"));
    assert_eq!(
        started.transcript_path.as_deref(),
        Some("/tmp/transcript_full.jsonl")
    );
    assert_eq!(started.launch.model.as_deref(), Some(FLASH));
    for (event, payload, expected) in [
        (
            "PreInvocation",
            with(&common, [("invocationNum", json!(1))]),
            None,
        ),
        (
            "PostToolUse:edit",
            with(&common, [("error", json!(""))]),
            Some(tool(true, true)),
        ),
        (
            "PostToolUse:mutating",
            with(&common, [("error", json!(null))]),
            Some(tool(true, false)),
        ),
        (
            "PostToolUse:edit",
            with(&common, [("error", json!("write failed"))]),
            Some(tool(false, false)),
        ),
        (
            "Stop",
            with(
                &common,
                [
                    ("terminationReason", json!("model_stop")),
                    ("error", json!("")),
                    ("fullyIdle", json!(false)),
                ],
            ),
            Some(ended(false, true)),
        ),
        (
            "Stop",
            with(
                &common,
                [
                    ("terminationReason", json!("max_steps_exceeded")),
                    ("fullyIdle", json!(true)),
                ],
            ),
            Some(ended(true, false)),
        ),
        (
            "Stop",
            with(&common, [("terminationReason", json!("model_stop"))]),
            None,
        ),
        ("Stop", json!({"fullyIdle": true}), None),
    ] {
        let signal = AntigravityAdapter
            .decode_hook(event, &payload)
            .expect("test hook decodes")
            .lifecycle
            .map(|value| value.signal);
        assert_eq!(signal, expected);
    }
}
#[test]
fn untyped_stop_errors_stay_terminal_and_cannot_arm_recovery() {
    for error in [
        json!("rate limit reached; retry later"),
        json!({"message": "quota exhausted", "retryAfterSeconds": 30}),
    ] {
        let payload = with(
            &hook_payload(),
            [
                ("terminationReason", json!("error")),
                ("fullyIdle", json!(true)),
                ("error", error),
            ],
        );
        assert!(
            AntigravityAdapter
                .decode_hook("Stop", &payload)
                .expect("test hook decodes")
                .turn_error
                .is_none()
        );
        assert_eq!(
            AntigravityAdapter
                .decode_hook("Stop", &payload)
                .expect("test hook decodes")
                .lifecycle
                .unwrap()
                .signal,
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            }
        );
    }
}
#[test]
fn hook_install_round_trips_existing_and_absent_statuslines() {
    for (name, original, change) in [
        (
            "wrapped",
            json_value(
                r#"{"colorScheme":"tokyo night","statusLine":{"type":"command","command":"my-statusline --compact","stack_with_default":false,"custom":"kept"}}"#,
            ),
            StatusLineChange::Wrapping {
                original: "my-statusline --compact".to_owned(),
            },
        ),
        (
            "added",
            json_value(r#"{"colorScheme":"tokyo night"}"#),
            StatusLineChange::Added,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let hooks_path = dir.path().join(format!("{name}/config/hooks.json"));
        let settings_path = dir.path().join(format!("{name}/settings.json"));
        std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
        std::fs::write(
            &hooks_path,
            r#"{"mine":{"Stop":[{"type":"command","command":"my-stop","timeout":9}]}}"#,
        )
        .unwrap();
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&original).unwrap(),
        )
        .unwrap();
        let preview = install::preview(&hooks_path, &settings_path).unwrap();
        assert_eq!(preview.planned_events, ANTIGRAVITY_EVENT_NAMES);
        assert_eq!(preview.status_line_change, Some(change));
        assert!(!preview.files[0].candidate.contains("PreToolUse"));
        install::install(&hooks_path, &settings_path).unwrap();
        assert!(install::installed(&hooks_path, &settings_path));
        let mut hooks = read_json(&hooks_path);
        assert_eq!(hooks["mine"]["Stop"][0]["command"], "my-stop");
        assert!(hooks["rimz"].get("PreToolUse").is_none());
        assert_eq!(hooks["rimz"]["PreInvocation"].as_array().unwrap().len(), 1);
        assert_eq!(hooks["rimz"]["PostToolUse"].as_array().unwrap().len(), 3);
        let installed = read_json(&settings_path);
        if name == "wrapped" {
            assert_eq!(
                install::wrapped_statusline_command(&settings_path).as_deref(),
                Some("my-statusline --compact")
            );
            assert_eq!(installed["statusLine"]["stack_with_default"], false);
            assert_eq!(installed["statusLine"]["_rimz_wrapped"]["custom"], "kept");
        } else {
            assert_eq!(installed["statusLine"]["command"], STATUS_LINE_COMMAND);
            assert_eq!(installed["statusLine"]["stack_with_default"], true);
        }
        hooks["rimz"]["Stop"][0]["timeout"] = json!(1);
        std::fs::write(&hooks_path, serde_json::to_string_pretty(&hooks).unwrap()).unwrap();
        assert!(!install::installed(&hooks_path, &settings_path));
        install::install(&hooks_path, &settings_path).unwrap();
        assert!(install::installed(&hooks_path, &settings_path));
        let once = (
            std::fs::read_to_string(&hooks_path).unwrap(),
            std::fs::read_to_string(&settings_path).unwrap(),
        );
        install::install(&hooks_path, &settings_path).unwrap();
        assert_eq!(std::fs::read_to_string(&hooks_path).unwrap(), once.0);
        assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), once.1);
        assert_eq!(
            install::uninstall(&hooks_path, &settings_path)
                .unwrap()
                .removed_events,
            ANTIGRAVITY_EVENT_NAMES
        );
        assert!(!install::managed(&hooks_path, &settings_path));
        let restored_hooks = read_json(&hooks_path);
        assert!(restored_hooks.get("rimz").is_none());
        assert_eq!(restored_hooks["mine"]["Stop"][0]["command"], "my-stop");
        assert_eq!(read_json(&settings_path), original);
    }
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
fn statusline_projects_model_account_and_context_usage() {
    let context = AntigravityAdapter
        .observe_context(
            "antigravity",
            &json_value(r#"{"conversation_id":"11111111-1111-4111-8111-111111111111","version":"1.1.2","model":{"id":"Gemini 3.5 Flash (Medium)","display_name":"Gemini 3.5 Flash (Medium)"},"plan_tier":"ultra","email":"user@example.com","tool_confirmation_pending":true,"context_window":{"context_window_size":1048576,"used_percentage":8.4156,"remaining_percentage":91.5844,"current_usage":{"input_tokens":63382,"output_tokens":346,"cache_creation_input_tokens":0,"cache_read_input_tokens":20857}},"future_field":{"ignored":true}}"#),
        )
        .unwrap();
    assert_eq!(
        context.model_id.as_deref(),
        Some("Gemini 3.5 Flash (Medium)")
    );
    assert_eq!(context.model_display_name.as_deref(), Some(FLASH));
    assert_eq!(context.agent_version.as_deref(), Some("1.1.2"));
    assert!(context.native_permission_wait.is_some());
    let account = context.account.unwrap();
    assert_eq!(
        (
            account.plan.as_deref(),
            account.account_id.as_deref(),
            account.metered
        ),
        (Some("ultra"), Some("user@example.com"), Some(true))
    );
    let tokens = context.tokens.unwrap();
    assert_eq!(
        (
            tokens.context_window_size,
            tokens.used_percentage,
            tokens.remaining_percentage,
            tokens.current_usage.unwrap().cache_read_input_tokens
        ),
        (Some(1_048_576), Some(8), Some(92), Some(20_857))
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
fn statusline_normalizes_reasoning_qualifiers_without_changing_model_id() {
    for (display, model, effort, thinking) in [
        ("Gemini 3.5 Flash (Low)", FLASH, Some("low"), None),
        ("Gemini 3.5 Flash (Medium)", FLASH, Some("medium"), None),
        ("Gemini 3.1 Pro (High)", PRO, Some("high"), None),
        ("Claude Sonnet 4.6 (Thinking)", SONNET, None, Some(true)),
        (
            "  Gemini 3.5 Flash (mEdIuM) \t",
            FLASH,
            Some("medium"),
            None,
        ),
        (TURBO, TURBO, None, None),
        (MALFORMED_MODEL, MALFORMED_MODEL, None, None),
    ] {
        let context = AntigravityAdapter
            .observe_context(
                "antigravity",
                &json!({"model": {"id": "  provider/model:id  ", "display_name": display}}),
            )
            .unwrap();
        assert_eq!(
            (
                context.model_id.as_deref(),
                context.model_display_name.as_deref(),
                context.effort.as_deref(),
                context.thinking_enabled
            ),
            (Some("  provider/model:id  "), Some(model), effort, thinking)
        );
    }
}
#[test]
fn statusline_prices_canonical_and_observed_model_ids_with_current_usage() {
    let prices = PriceBook::from_litellm_json(
        r#"{"agy-priced":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"cache_creation_input_token_cost":3e-6,"cache_read_input_token_cost":0.25e-6},"gemini-3.5-flash":{"input_cost_per_token":1.5e-6,"output_cost_per_token":9e-6,"cache_creation_input_token_cost":1.875e-6,"cache_read_input_token_cost":0.15e-6}}"#,
    );
    for (id, usage, expected) in [
        ("  agy-priced-via-router  ", (10, 20, 30, 40), 150e-6),
        (
            "Gemini 3.5 Flash (Medium)",
            (2_971, 630, 0, 16_270),
            0.012_567,
        ),
    ] {
        let cost = AntigravityAdapter
            .context_cost(
                &json!({
                    "model": {"id": id, "display_name": id},
                    "context_window": {"current_usage": {
                        "input_tokens": usage.0,
                        "output_tokens": usage.1,
                        "cache_creation_input_tokens": usage.2,
                        "cache_read_input_tokens": usage.3
                    }}
                }),
                &prices,
            )
            .unwrap();
        assert_eq!(cost.coverage, crate::agents::CostCoverage::CurrentUsage);
        assert!((cost.total_cost_usd.unwrap() - expected).abs() < 1e-15);
    }
    for payload in [
        r#"{"model":{"id":"unknown","display_name":"agy-priced"},"context_window":{"current_usage":{"input_tokens":10}}}"#,
        r#"{"model":{"id":"agy-priced"}}"#,
        r#"{"model":{"id":"agy-priced"},"context_window":{"current_usage":{"input_tokens":0}}}"#,
        r#"{"model":{"id":"   "},"context_window":{"current_usage":{"input_tokens":10}}}"#,
        r#"{"model":{"id":"Gemini 3.5 Flash Turbo (Medium)"},"context_window":{"current_usage":{"input_tokens":10}}}"#,
    ] {
        assert!(
            AntigravityAdapter
                .context_cost(&json_value(payload), &prices)
                .is_none()
        );
    }
}
#[test]
fn visible_transcript_contract_exposes_only_normalized_messages() {
    let messages = AntigravityAdapter
        .parse_transcript_messages(include_str!("tests/fixtures/transcript_full.jsonl"));
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (TranscriptRole::User, "ping"),
            (TranscriptRole::Assistant, "pong")
        ]
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("checkpoint"))
    );
    let wrapped = "  <USER_REQUEST>\nrefine the card\nwithout metadata\n</USER_REQUEST>\n<ADDITIONAL_METADATA>{\"secret\":true}</ADDITIONAL_METADATA>  ";
    let lines = [
        user_record(0, AT_09, wrapped),
        user_record(1, AT_10, "<USER_REQUEST>missing close"),
        user_record(2, AT_11, "<USER_REQUEST> </USER_REQUEST>"),
        user_record(3, AT_12, "  legacy prompt  "),
    ]
    .join("\n");
    assert_eq!(
        session::messages(&lines)
            .into_iter()
            .map(|message| message.text)
            .collect::<Vec<_>>(),
        ["refine the card\nwithout metadata", "legacy prompt"]
    );
    assert!(
        AntigravityAdapter
            .parse_transcript_messages(concat!(
                "not-json\n{}\n",
                r#"{"step_index":4,"source":"MODEL","type":"PLANNER_THOUGHT","status":"DONE","created_at":"2026-07-13T23:23:13Z","content":"hidden"}"#,
                "\n",
                r#"{"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"IN_PROGRESS","created_at":"2026-07-13T23:23:14Z","content":"partial"}"#,
            ))
            .is_empty()
    );
    let dir = tempfile::tempdir().unwrap();
    let path = write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", "");
    for (content, expected) in [
        (wrapped, Some("refine the card\nwithout metadata")),
        ("<USER_REQUEST>missing close", None),
        ("<USER_REQUEST> </USER_REQUEST><SETTINGS />", None),
        ("  legacy prompt  ", Some("legacy prompt")),
    ] {
        std::fs::write(&path, user_record(0, AT_09, content)).unwrap();
        assert_eq!(
            session::latest_prompt_under(dir.path(), &path, SESSION_ID).as_deref(),
            expected
        );
    }
}

#[test]
fn final_assistant_uses_tail_then_full_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let assistant = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-07-13T23:23:09Z","content":"tail answer"}"#;
    let padding = format!(
        "{{\"step_index\":2,\"source\":\"SYSTEM\",\"type\":\"CHECKPOINT\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:10Z\",\"content\":{}}}",
        serde_json::to_string(&"x".repeat(70_000)).unwrap()
    );
    std::fs::write(&path, format!("{padding}\n{assistant}\n")).unwrap();
    assert_eq!(
        session::last_assistant_message(&path).as_deref(),
        Some("tail answer")
    );
    std::fs::write(&path, format!("{assistant}\n{padding}\n")).unwrap();
    assert_eq!(
        session::last_assistant_message(&path).as_deref(),
        Some("tail answer")
    );
}
#[test]
fn transcript_questions_project_and_clear_native_waits() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let array_questions = json_value(
        r#"[{"name":"ask_question","args":{"questions":[{"question":"  First choice?  ","options":["a","b"]},{"question":"Second choice?","options":[]}]}}]"#,
    );
    let encoded_questions = json_value(
        r#"[{"name":"ask_question","args":{"questions":"[{\"question\":\"Replacement?\",\"options\":[\"yes\",\"no\"]}]"}}]"#,
    );
    let ask_again =
        json_value(r#"[{"name":"ask_question","args":{"questions":[{"question":"One more?"}]}}]"#);
    let waiting = |detail, at| {
        (
            AgentStatus::Waiting,
            TurnPhase::Idle,
            Some(detail),
            Some(at),
        )
    };
    for (ending, expected) in [
        (
            vec![planner_record(1, AT_10, Some(array_questions))],
            waiting("First choice?", AT_10),
        ),
        (
            vec![planner_record(1, AT_11, Some(encoded_questions))],
            waiting("Replacement?", AT_11),
        ),
        (
            vec![
                planner_record(1, AT_10, Some(ask_again.clone())),
                planner_record(2, AT_11, None),
            ],
            (AgentStatus::Success, TurnPhase::Idle, None, None),
        ),
        (
            vec![
                planner_record(1, AT_10, Some(ask_again)),
                user_record(2, AT_11, "continue"),
            ],
            (AgentStatus::Running, TurnPhase::Reasoning, None, None),
        ),
    ] {
        let state = state_after(dir.path(), workspace, ending);
        assert_eq!(
            (
                state.status,
                state.phase,
                state.native_prompt_detail.as_deref(),
                state.waiting_since
            ),
            (
                expected.0,
                expected.1,
                expected.2,
                expected.3.map(|at| at.parse().unwrap())
            )
        );
    }
    for calls in [
        r#""not-an-array""#,
        r#"[{"name":"ask_question","args":{"questions":"not-json"}}]"#,
        r#"[{"name":"ask_question","args":{"questions":[]}}]"#,
        r#"[{"name":"another_tool","args":{"questions":[{"question":"ignored"}]}}]"#,
    ] {
        let ending = vec![planner_record(1, AT_10, Some(json_value(calls)))];
        let state = state_after(dir.path(), workspace, ending);
        assert_eq!(state.status, AgentStatus::Success);
        assert_eq!(state.phase, TurnPhase::Idle);
        assert!(state.native_prompt_detail.is_none() && state.waiting_since.is_none());
    }
}

#[test]
fn invoke_subagent_results_pair_ordered_child_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let parent = write_subagent_fixture(
        dir.path(),
        SESSION_ID,
        include_str!("tests/fixtures/subagents_two_children.jsonl"),
    );
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }

    let alpha = session::correlate_subagent_under(
        dir.path(),
        &parent,
        SESSION_ID,
        &workspace,
        CHILD_ALPHA,
        &workspace,
    )
    .unwrap();
    let beta = session::correlate_subagent_under(
        dir.path(),
        &parent,
        SESSION_ID,
        &workspace,
        CHILD_BETA,
        &workspace,
    )
    .unwrap();
    let alpha_prompt = format!(
        "Read the security policy in {}/SECURITY.md and provide a concise summary of the reporting process.",
        workspace.display()
    );
    let beta_prompt = format!(
        "Search {}/AGENTS.md for references to nextest and summarize the testing requirements related to it.",
        workspace.display()
    );
    let spawned = session::spawned_subagents_under(dir.path(), &parent, SESSION_ID, &workspace);
    assert_eq!(
        spawned
            .iter()
            .map(|child| (
                child.child_agent_id.as_str(),
                child.agent_name.as_deref(),
                child.role.as_deref(),
                child.prompt.as_deref(),
            ))
            .collect::<Vec<_>>(),
        [
            (
                CHILD_ALPHA,
                Some("summary_agent"),
                Some("Security Document Summarizer"),
                Some(alpha_prompt.as_str()),
            ),
            (
                CHILD_BETA,
                Some("research"),
                Some("Codebase Researcher"),
                Some(beta_prompt.as_str()),
            ),
        ]
    );
    assert_eq!(
        (
            alpha.type_name.as_str(),
            alpha.role.as_str(),
            alpha.prompt.as_str()
        ),
        (
            "summary_agent",
            "Security Document Summarizer",
            alpha_prompt.as_str()
        )
    );
    assert_eq!(
        (
            beta.type_name.as_str(),
            beta.role.as_str(),
            beta.prompt.as_str()
        ),
        ("research", "Codebase Researcher", beta_prompt.as_str())
    );
}

#[test]
fn nested_child_relation_uses_its_immediate_parent_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let parent = write_subagent_fixture(
        dir.path(),
        CHILD_ALPHA,
        include_str!("tests/fixtures/subagent_nested_child.jsonl"),
    );
    write_transcript_named(
        dir.path(),
        NESTED_CHILD,
        "transcript.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );

    let nested = session::correlate_subagent_under(
        dir.path(),
        &parent,
        CHILD_ALPHA,
        &workspace,
        NESTED_CHILD,
        &workspace,
    )
    .unwrap();
    assert_eq!(nested.type_name, "explore");
    assert_eq!(nested.role, "Nested researcher");
    assert_eq!(nested.prompt, "Trace the nested call.");
    let spawned = session::spawned_subagents_under(dir.path(), &parent, CHILD_ALPHA, &workspace);
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].child_agent_id.as_str(), NESTED_CHILD);
    assert_eq!(spawned[0].agent_name.as_deref(), Some("explore"));
    assert_eq!(spawned[0].role.as_deref(), Some("Nested researcher"));
    assert_eq!(spawned[0].prompt.as_deref(), Some("Trace the nested call."));
}

#[test]
fn subagent_correlation_rejects_unsafe_identity_uri_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let other_workspace = dir.path().join("other-workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&other_workspace).unwrap();
    let fixture = include_str!("tests/fixtures/subagents_two_children.jsonl");
    let parent = write_subagent_fixture(dir.path(), SESSION_ID, fixture);
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }
    let correlate = |path: &Path, parent_id: &str, child_workspace: &Path| {
        session::correlate_subagent_under(
            dir.path(),
            path,
            parent_id,
            &workspace,
            CHILD_ALPHA,
            child_workspace,
        )
    };
    assert!(correlate(&parent, CHILD_ALPHA, &workspace).is_none());
    assert!(correlate(&parent, SESSION_ID, &other_workspace).is_none());

    let escaped = rewrite_subagent_result(fixture, |content| {
        content.replace(
            &format!("brain/{CHILD_ALPHA}/.system_generated/logs/transcript.jsonl"),
            &format!("brain/{CHILD_BETA}/.system_generated/logs/transcript.jsonl"),
        )
    });
    let escaped_parent = write_subagent_fixture(dir.path(), SESSION_ID, &escaped);
    assert!(correlate(&escaped_parent, SESSION_ID, &workspace).is_none());

    let remote = rewrite_subagent_result(fixture, |content| {
        content.replacen("__HOME_URI__", "https://example.invalid", 1)
    });
    let remote_parent = write_subagent_fixture(dir.path(), SESSION_ID, &remote);
    assert!(correlate(&remote_parent, SESSION_ID, &workspace).is_none());

    let remote_workspace = fixture.replace("__WORKSPACE_URI__", "https://example.invalid");
    let remote_workspace_parent = write_subagent_fixture(dir.path(), SESSION_ID, &remote_workspace);
    assert!(correlate(&remote_workspace_parent, SESSION_ID, &workspace).is_none());

    let relative_workspace = fixture.replacen(
        "\"TypeName\":\"summary_agent\"}",
        "\"TypeName\":\"summary_agent\",\"Workspace\":\"relative\"}",
        1,
    );
    let relative_parent = write_subagent_fixture(dir.path(), SESSION_ID, &relative_workspace);
    assert!(correlate(&relative_parent, SESSION_ID, &workspace).is_none());
}

#[test]
fn subagent_correlation_rejects_torn_mismatched_and_duplicate_results() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }
    let fixture = include_str!("tests/fixtures/subagents_two_children.jsonl");
    let assert_rejected = |fixture: &str| {
        let parent = write_subagent_fixture(dir.path(), SESSION_ID, fixture);
        assert!(
            session::correlate_subagent_under(
                dir.path(),
                &parent,
                SESSION_ID,
                &workspace,
                CHILD_ALPHA,
                &workspace,
            )
            .is_none()
        );
        assert!(
            session::spawned_subagents_under(dir.path(), &parent, SESSION_ID, &workspace,)
                .is_empty()
        );
    };
    let mismatched = rewrite_subagent_result(fixture, |content| {
        content
            .split_once(&format!("{{\n  \"conversationId\": \"{CHILD_BETA}\""))
            .map(|(before, _)| before.to_owned())
            .unwrap()
    });
    assert_rejected(&mismatched);
    let extra = rewrite_subagent_result(fixture, |content| {
        format!(
            "{content}\n{{\"conversationId\":\"extra-child\",\"logAbsoluteUri\":\"file:///tmp/extra.jsonl\"}}"
        )
    });
    assert_rejected(&extra);
    let duplicate = fixture.replace(CHILD_BETA, CHILD_ALPHA);
    assert_rejected(&duplicate);
    let malformed = rewrite_subagent_result(fixture, |content| {
        content.replacen("\"logAbsoluteUri\"", "\"missingLogUri\"", 1)
    });
    assert_rejected(&malformed);

    let parent = write_subagent_fixture(dir.path(), SESSION_ID, fixture);
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&parent)
        .unwrap();
    write!(file, "\n{{\"step_index\":3").unwrap();
    assert!(
        session::correlate_subagent_under(
            dir.path(),
            &parent,
            SESSION_ID,
            &workspace,
            CHILD_ALPHA,
            &workspace,
        )
        .is_none()
    );
    assert!(
        session::spawned_subagents_under(dir.path(), &parent, SESSION_ID, &workspace,).is_empty()
    );
}

#[test]
fn subagent_correlation_does_not_scan_beyond_the_transcript_tail() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    write_transcript_named(
        dir.path(),
        CHILD_ALPHA,
        "transcript.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    write_transcript_named(
        dir.path(),
        CHILD_BETA,
        "transcript.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    let mut records = include_str!("tests/fixtures/subagents_two_children.jsonl")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    records.insert(
        2,
        json!({
            "step_index": 99,
            "source": "SYSTEM",
            "type": "CHECKPOINT",
            "status": "DONE",
            "created_at": AT_12,
            "content": "x".repeat(70 * 1024),
        })
        .to_string(),
    );
    let parent = write_subagent_fixture(dir.path(), SESSION_ID, &records.join("\n"));
    assert!(std::fs::metadata(&parent).unwrap().len() > 64 * 1024);
    assert!(
        session::correlate_subagent_under(
            dir.path(),
            &parent,
            SESSION_ID,
            &workspace,
            CHILD_ALPHA,
            &workspace,
        )
        .is_none()
    );
}
#[test]
fn discovery_prefers_full_transcripts_and_cache_for_fresh_pairing_only() {
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
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::fs::write(
        dir.path().join("cache/last_conversations.json"),
        json!({workspace.to_str().unwrap(): SESSION_ID}).to_string(),
    )
    .unwrap();
    let observations = session::discover_under(dir.path(), &workspace);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    let at = AT_09.parse().unwrap();
    assert_eq!(
        (
            observation.session_id.as_str(),
            observation.transcript_path.as_path(),
            observation.first_event_at,
            observation.last_activity,
            observation.fresh_binding_at,
            local_state(observation).latest_prompt.as_deref()
        ),
        (
            SESSION_ID,
            full.as_path(),
            Some(at),
            at,
            Some(at),
            Some("ping")
        )
    );
    let other = dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let exact_resume_only = session::discover_under(dir.path(), &other);
    assert_eq!(exact_resume_only.len(), 1);
    assert!(exact_resume_only[0].fresh_binding_at.is_none());
    let unknown = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_debug.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    assert!(session::latest_prompt_under(dir.path(), &unknown, SESSION_ID).is_none());
    assert!(session::latest_prompt_under(dir.path(), &full, "other-conversation").is_none());
    assert_eq!(
        session::resolve_home(Some(OsStr::new("/tmp/agy")), Some(OsStr::new("/home/user"))),
        Some(Path::new("/tmp/agy").to_path_buf())
    );
    assert_eq!(
        session::resolve_home(None, Some(OsStr::new("/home/user"))),
        Some(Path::new("/home/user/.gemini/antigravity-cli").to_path_buf())
    );
}
#[test]
fn discovery_cache_batches_workspaces_and_revalidates_changed_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let transcript = write_transcript(dir.path(), SESSION_ID);
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    let index = dir.path().join("cache/last_conversations.json");
    std::fs::write(
        &index,
        json!({first.to_str().unwrap(): SESSION_ID}).to_string(),
    )
    .unwrap();
    let mut cache = session::DiscoveryCacheHarness::new();
    let start = Instant::now();

    assert_eq!(
        cache
            .refresh(dir.path(), &[first.as_path(), second.as_path()], start)
            .len(),
        2
    );
    assert_eq!(cache.work(), (1, 1, 1));
    assert_eq!(
        cache
            .refresh(dir.path(), &[first.as_path(), second.as_path()], start)
            .len(),
        2
    );
    assert_eq!(cache.work(), (1, 1, 1));

    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(transcript)
            .unwrap(),
        "{}",
        user_record(9, "2026-07-13T23:24:00Z", "changed")
    )
    .unwrap();
    assert_eq!(
        cache
            .refresh(dir.path(), &[first.as_path(), second.as_path()], start)
            .len(),
        2
    );
    assert_eq!(cache.work(), (1, 1, 2));

    std::fs::write(
        index,
        json!({second.to_str().unwrap(): SESSION_ID}).to_string(),
    )
    .unwrap();
    cache.refresh(dir.path(), &[first.as_path(), second.as_path()], start);
    assert_eq!(cache.work(), (1, 2, 2));

    cache.refresh(
        dir.path(),
        &[first.as_path(), second.as_path()],
        start + Duration::from_secs(30),
    );
    assert_eq!(cache.work(), (2, 3, 3));
}
#[test]
fn bounded_discovery_can_lose_an_older_prompt_from_a_long_turn() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut records = vec![user_record(0, "2026-07-13T23:00:00Z", "sticky label")];
    for step in 1..=900 {
        records.push(planner_record(step, AT_10, Some(json!([]))));
    }
    let transcript = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &records.join("\n"),
    );
    assert!(std::fs::metadata(&transcript).unwrap().len() > 64 * 1024);
    let observation = session::discover_under(dir.path(), &workspace).remove(0);
    assert_eq!(local_state(&observation).status, AgentStatus::Success);
    assert!(local_state(&observation).latest_prompt.is_none());
}
#[test]
fn first_invocation_recovers_only_the_latest_completed_visible_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let torn = user_record(5, "2026-07-13T23:23:13Z", "torn");
    let mut lines = vec!["not-json".to_owned()];
    for (step, source, kind, status, at, content) in [
        (
            0,
            "SYSTEM",
            "CHECKPOINT",
            "DONE",
            "2026-07-13T23:23:08Z",
            "internal description",
        ),
        (
            1,
            "USER_EXPLICIT",
            "USER_INPUT",
            "IN_PROGRESS",
            AT_10,
            "partial description",
        ),
        (2, "MODEL", "USER_INPUT", "DONE", AT_11, "wrong source"),
    ] {
        lines.push(record(step, source, kind, status, at, content).to_string());
    }
    lines.extend([
        user_record(4, AT_12, "  label this turn  "),
        torn[..torn.len() / 2].to_owned(),
    ]);
    let transcript = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &lines.join("\n"),
    );
    let payload = with(
        &hook_payload(),
        [
            ("transcriptPath", json!(transcript)),
            ("invocationNum", json!(0)),
        ],
    );
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
        |_, _| panic!("Stop must not read transcripts"),
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
    let transcript = escaped
        .path()
        .join(".system_generated/logs/transcript_full.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        include_str!("tests/fixtures/transcript_full.jsonl"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("brain")).unwrap();
    symlink(escaped.path(), dir.path().join("brain").join(SESSION_ID)).unwrap();
    assert!(session::discover_under(dir.path(), Path::new("/workspace/project")).is_empty());
}
#[test]
fn launch_resume_permissions_and_model_preset_match_agy() {
    let extra = ["--sandbox".to_owned()];
    assert_eq!(
        AntigravityAdapter
            .launch_command(&extra, Some("review"))
            .unwrap(),
        ["agy", "--sandbox", "--prompt-interactive", "review"]
    );
    assert_eq!(
        AntigravityAdapter
            .resume_command(SESSION_ID, Path::new("/workspace/project"))
            .unwrap(),
        ["agy", "--conversation", SESSION_ID]
    );
    for (mode, expected) in [
        (PermissionMode::Auto, &["--mode", "accept-edits"][..]),
        (PermissionMode::Plan, &["--mode", "plan"][..]),
        (
            PermissionMode::Yolo,
            &["--dangerously-skip-permissions"][..],
        ),
    ] {
        assert_eq!(
            AntigravityAdapter.descriptor().launch.permission_args(mode),
            expected
        );
    }
    assert_eq!(
        AntigravityAdapter
            .descriptor()
            .render_preset(&LaunchPreset {
                model: Some("Gemini 3.5 Flash (Low)".to_owned()),
                ..Default::default()
            })
            .unwrap(),
        ["--model", "Gemini 3.5 Flash (Low)"]
    );
    assert!(
        AntigravityAdapter
            .descriptor()
            .render_preset(&LaunchPreset {
                effort: Some("high".to_owned()),
                ..Default::default()
            })
            .is_err()
    );
    for (command, expected) in [
        (
            "agy --conversation 11111111-1111-4111-8111-111111111111",
            Some(SESSION_ID),
        ),
        (
            "/home/user/.local/bin/agy --conversation=11111111-1111-4111-8111-111111111111",
            Some(SESSION_ID),
        ),
        ("agy --continue", None),
        ("agy -c", None),
        ("agy --conversation=", None),
        ("agy --conversation --mode plan", None),
        ("agy", None),
        (
            "echo agy --conversation 11111111-1111-4111-8111-111111111111",
            None,
        ),
    ] {
        let resumed = AntigravityAdapter.resumed_session_id_from_cmdline(command);
        assert_eq!(resumed.as_deref(), expected);
    }
}
mod local_account_api {
    use super::super::local_api::{self, LoopbackEndpoint};
    use super::*;
    use jiff::Timestamp;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::time::Duration;
    const OBSERVED_AT: &str = "2026-06-15T08:00:00Z";
    const FIVE_HOUR_RESET: &str = "2026-06-15T12:00:00Z";
    const WEEK_RESET: &str = "2026-06-20T08:00:00Z";
    fn observed_at() -> Timestamp {
        OBSERVED_AT.parse().unwrap()
    }
    #[test]
    fn process_identity_requires_exact_binary_argv_uid_and_start_token() {
        use local_api::process::{candidate_identity_matches, identity_matches};
        let agy = Path::new("/home/me/.local/bin/agy");
        let argv = OsStr::new("agy");
        for (uid, expected_uid, executable, argv0, expected) in [
            (Some(501), 501, agy, agy.as_os_str(), true),
            (Some(502), 501, agy, argv, false),
            (Some(501), 501, Path::new("/usr/bin/sh"), argv, false),
            (Some(501), 501, agy, OsStr::new("wrapper"), false),
        ] {
            assert_eq!(
                identity_matches(uid, expected_uid, executable, argv0),
                expected
            );
        }
        let candidate = local_api::Candidate {
            pid: 42,
            uid: 501,
            start_token: "1000".to_owned(),
            endpoints: Vec::new(),
        };
        for (token, expected) in [(Some("1000"), true), (Some("1001"), false)] {
            assert_eq!(
                candidate_identity_matches(&candidate, Some(501), agy, argv, token,),
                expected
            );
        }
    }
    #[test]
    fn socket_parsers_accept_process_owned_loopback_listeners_only() {
        let inodes = BTreeSet::from(["12345".to_owned(), "67890".to_owned()]);
        let tcp = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n\
   0: 0100007F:1F90 00000000:0000 0A 0:0 00:0 0 501 0 12345\n\
   1: 00000000:2382 00000000:0000 0A 0:0 00:0 0 501 0 67890\n\
   2: 0100007F:2383 00000000:0000 01 0:0 00:0 0 501 0 67890\n\
   3: 0100007F:2384 00000000:0000 0A 0:0 00:0 0 501 0 99999";
        let ipv4 = local_api::process::parse_proc_net(tcp, &inodes, false);
        assert_eq!(
            (ipv4[0].address, ipv4[0].port),
            (IpAddr::V4(Ipv4Addr::LOCALHOST), 8080)
        );
        assert_eq!(ipv4.len(), 1);
        let tcp6 = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n\
   0: 00000000000000000000000001000000:18EB 00000000000000000000000000000000:0000 0A 0:0 00:0 0 501 0 67890";
        let ipv6 = local_api::process::parse_proc_net(tcp6, &inodes, true);
        assert_eq!(
            (ipv6[0].address, ipv6[0].port),
            (IpAddr::V6(Ipv6Addr::LOCALHOST), 6379)
        );
        assert_eq!(ipv6.len(), 1);
        assert_eq!(
            local_api::process::socket_inode("socket:[12345]").as_deref(),
            Some("12345")
        );
        assert!(local_api::process::socket_inode("pipe:[12345]").is_none());
        let endpoints = local_api::process::parse_lsof(
            "p42\nn127.0.0.1:5000\nn[::1]:5001\nn0.0.0.0:5002\nn10.0.0.2:5003\n",
        );
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].url("/status"), "https://127.0.0.1:5000/status");
        assert_eq!(endpoints[1].url("/status"), "https://[::1]:5001/status");
    }
    #[test]
    fn identity_status_normalizes_owner_plan_and_success_wire() {
        let base = json_value(
            r#"{"userStatus":{"email":" user@example.com ","userTier":{"name":" Google AI Ultra "},"planStatus":{"planInfo":{"planName":"Pro"}}}}"#,
        );
        for code in [json!(0), json!("success")] {
            let account =
                local_api::wire::parse_identity(&with(&base, [("code", code)]).to_string())
                    .unwrap();
            assert_eq!(account.account_id.as_deref(), Some("user@example.com"));
            assert_eq!(account.plan.as_deref(), Some("Google AI Ultra"));
            assert_eq!(account.metered, Some(true));
        }
        let fallback = local_api::wire::parse_identity(
            r#"{"userStatus":{"planStatus":{"planInfo":{"displayName":" Antigravity Pro "}}}}"#,
        )
        .unwrap();
        assert_eq!(fallback.plan.as_deref(), Some("Antigravity Pro"));
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
    fn account_keys_bind_normalized_owners_and_preserve_plan() {
        let expected =
            "antigravity:v1:03036c73f6a2c8d3a1461c99f6f41c04e8b66e84d1d437f234387c73fd50a726";
        for email in ["user@example.com", " USER@EXAMPLE.COM "] {
            let key = local_api::wire::account_key(email).unwrap();
            assert_eq!(key, expected);
            assert!(!key.contains("user@example.com"));
        }
        assert_ne!(
            local_api::wire::account_key("user@example.com"),
            local_api::wire::account_key("other@example.com")
        );
        assert_eq!(local_api::wire::account_key("  "), None);
        let body = json_value(
            r#"{"userStatus":{"email":" User@Example.com ","userTier":{"name":" Google AI Ultra "}}}"#,
        );
        let (identity, plan) =
            local_api::wire::parse_account_usage_identity(&body.to_string()).unwrap();
        assert_eq!(identity.account_key.as_deref(), Some(expected));
        assert_eq!(
            identity.scope,
            crate::agents::ProviderAccountScope::KindWide
        );
        assert_eq!(plan.as_deref(), Some("Google AI Ultra"));
        let plan_only = json!({"userStatus": {"userTier": {"name": "Pro"}}});
        assert!(local_api::wire::parse_identity(&plan_only.to_string()).is_ok());
        assert!(local_api::wire::parse_account_usage_identity(&plan_only.to_string()).is_err());
    }
    #[test]
    fn quota_envelopes_and_fraction_shapes_choose_strictest_windows() {
        let groups = json_value(
            r#"[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":0.8,"resetTime":"2026-06-15T12:00:00Z"},{"bucketId":"3p-5h","remaining":{"remainingFraction":0.3},"resetTime":"2026-06-15T11:00:00Z"},{"bucketId":"other-5h","remainingFraction":0.3,"resetTime":"2026-06-15T12:00:00Z"},{"bucketId":"gemini-weekly","remaining":{"case":"remainingFraction","value":0.4},"resetTime":"2026-06-20T08:00:00Z"},{"bucketId":"3p-weekly","remaining":{"remainingFraction":0.6},"resetTime":"2026-06-21T08:00:00Z"}]}]"#,
        );
        for body in [
            json!({"response": {"groups": groups.clone()}}),
            json!({"summary": {"groups": groups.clone()}}),
            json!({"groups": groups.clone()}),
        ] {
            let limits =
                local_api::wire::parse_rate_limits(&body.to_string(), observed_at()).unwrap();
            assert_eq!(limits.windows.len(), 2);
            assert_eq!(
                (
                    limits.windows[0].duration_mins,
                    limits.windows[0].used_percentage,
                    limits.windows[0].resets_at,
                    limits.windows[1].duration_mins,
                    limits.windows[1].used_percentage,
                    limits.windows[1].resets_at
                ),
                (
                    Some(300),
                    Some(70),
                    Some(FIVE_HOUR_RESET.parse().unwrap()),
                    Some(10_080),
                    Some(60),
                    Some(WEEK_RESET.parse().unwrap())
                )
            );
            assert!(limits.windows.iter().all(|window| {
                window.source == crate::agents::context::WindowSource::Authoritative
                    && !window.lifted
            }));
        }
    }
    #[test]
    fn quota_preserves_empty_windows_and_detects_display_name_periods() {
        let body = json_value(
            r#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","disabled":true,"remainingFraction":0.5},{"bucketId":"3p-5h"},{"displayName":"Weekly Limit","remainingFraction":0.001,"resetTime":"2026-06-20T08:00:00Z"}]}]}"#,
        );
        let limits = local_api::wire::parse_rate_limits(&body.to_string(), observed_at()).unwrap();
        assert_eq!(limits.windows.len(), 2);
        assert_eq!(
            (
                limits.windows[0].used_percentage,
                limits.windows[0].resets_at,
                limits.windows[1].used_percentage
            ),
            (None, None, Some(99))
        );
        let exhausted = json_value(
            r#"{"groups":[{"buckets":[{"bucketId":"gemini-weekly","remainingFraction":0,"resetTime":"2026-06-20T08:00:00Z"}]}]}"#,
        );
        let limits =
            local_api::wire::parse_rate_limits(&exhausted.to_string(), observed_at()).unwrap();
        assert_eq!(limits.windows.len(), 1);
        assert_eq!(limits.windows[0].used_percentage, Some(100));
    }
    #[test]
    fn quota_rejects_malformed_authoritative_and_unknown_only_data() {
        for bucket in [
            json!({"bucketId": "gemini-5h", "remainingFraction": -0.1, "resetTime": FIVE_HOUR_RESET}),
            json!({"bucketId": "gemini-5h", "remainingFraction": 0.5, "resetTime": "invalid"}),
            json!({"bucketId": "gemini-5h", "remainingFraction": 0.5, "resetTime": OBSERVED_AT}),
        ] {
            let body = json!({"groups": [{"buckets": [
                bucket,
                {"bucketId": "3p-5h", "remainingFraction": 0.9, "resetTime": FIVE_HOUR_RESET}
            ]}]});
            assert!(local_api::wire::parse_rate_limits(&body.to_string(), observed_at()).is_err());
        }
        let unknown = json_value(
            r#"{"groups":[{"buckets":[{"bucketId":"legacy-model-quota","remainingFraction":0.5,"resetTime":"2026-06-15T12:00:00Z"}]}]}"#,
        );
        assert!(local_api::wire::parse_rate_limits(&unknown.to_string(), observed_at()).is_err());
    }
    fn candidate(pid: u32, ports: &[u16]) -> local_api::Candidate {
        local_api::Candidate {
            pid,
            uid: 501,
            start_token: format!("start-{pid}"),
            endpoints: ports
                .iter()
                .map(|port| LoopbackEndpoint {
                    address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    port: *port,
                })
                .collect(),
        }
    }
    fn status_body(email: Option<&str>, plan: &str) -> String {
        json!({"userStatus": {"email": email, "userTier": {"name": plan}}}).to_string()
    }
    fn quota_body() -> String {
        r#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":0.75,"resetTime":"2099-06-15T12:00:00Z"},{"bucketId":"gemini-weekly","remainingFraction":0.5,"resetTime":"2099-06-20T08:00:00Z"}]}]}"#.to_owned()
    }
    #[test]
    fn paired_usage_probe_revalidates_one_endpoint_for_both_rpcs() {
        let mut discovery_calls = 0;
        let mut revalidated = Vec::new();
        let mut requests = Vec::new();
        let probe = local_api::probe_account_usage_with(
            |_| {
                discovery_calls += 1;
                Ok(vec![candidate(42, &[5000, 5001])])
            },
            |candidate| {
                revalidated.push(candidate.pid);
                Ok(())
            },
            |endpoint, path, body, timeout| {
                requests.push((endpoint, path.to_owned(), body.to_owned(), timeout));
                if path.ends_with("GetUserStatus") {
                    Ok(status_body(Some("user@example.com"), "Pro"))
                } else {
                    Ok(quota_body())
                }
            },
        );
        let crate::agents::AccountUsageProbe::Found { identity, snapshot } = probe else {
            panic!("expected paired account usage")
        };
        assert_eq!(discovery_calls, 1);
        assert_eq!(revalidated, [42, 42]);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.0.port == 5000));
        assert_eq!(
            requests[0].1,
            "/exa.language_server_pb.LanguageServerService/GetUserStatus"
        );
        assert_eq!(requests[0].2, "{}");
        assert_eq!(
            requests[1].1,
            "/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary"
        );
        assert_eq!(requests[1].2, r#"{"forceRefresh":true}"#);
        assert!(
            requests
                .iter()
                .all(|request| request.3 <= Duration::from_millis(750))
        );
        assert_eq!(snapshot.plan.as_deref(), Some("Pro"));
        assert_eq!(
            (snapshot.extra_credits, snapshot.reset_credits),
            (None, None)
        );
        assert_eq!(snapshot.rate_limits.unwrap().windows.len(), 2);
        assert!(identity.account_key.is_some());
        assert!(
            AntigravityAdapter
                .descriptor()
                .capabilities
                .direct_account_usage
        );
    }
    #[test]
    fn known_owner_quota_failure_stays_attributed_without_endpoint_fallback() {
        let mut requests = Vec::new();
        let probe = local_api::probe_account_usage_with(
            |_| Ok(vec![candidate(42, &[5000, 5001]), candidate(41, &[6000])]),
            |_| Ok(()),
            |endpoint, path, _, _| {
                requests.push((endpoint.port, path.to_owned()));
                if path.ends_with("GetUserStatus") {
                    Ok(status_body(Some("new@example.com"), "Pro"))
                } else {
                    Err(local_api::LocalApiError::Transport)
                }
            },
        );
        let crate::agents::AccountUsageProbe::Failed(identity) = probe else {
            panic!("known-owner partial failure must stay attributable")
        };
        assert!(identity.account_key.is_some());
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.0 == 5000));
    }
    #[test]
    fn ownerless_discovery_and_status_failures_remain_unattributed() {
        let discovery_failure = local_api::probe_account_usage_with(
            |_| Err(local_api::LocalApiError::Unavailable),
            |_| Ok(()),
            |_, _, _, _| Ok(String::new()),
        );
        assert_eq!(
            discovery_failure,
            crate::agents::AccountUsageProbe::Failed(Default::default())
        );
        let mut status_requests = 0;
        let status_failure = local_api::probe_account_usage_with(
            |_| Ok(vec![candidate(42, &[5000, 5001])]),
            |_| Ok(()),
            |_, _, _, _| {
                status_requests += 1;
                Ok(status_body(None, "Pro"))
            },
        );
        assert_eq!(status_requests, 2);
        assert_eq!(
            status_failure,
            crate::agents::AccountUsageProbe::Failed(Default::default())
        );
    }
}
fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
fn json_value(source: &str) -> Value {
    serde_json::from_str(source).unwrap()
}
fn write_transcript(home: &Path, session_id: &str) -> std::path::PathBuf {
    write_transcript_named(
        home,
        session_id,
        "transcript_full.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    )
}
fn write_subagent_fixture(home: &Path, session_id: &str, fixture: &str) -> std::path::PathBuf {
    let home_uri = url::Url::from_directory_path(home).unwrap().to_string();
    let workspace = home.join("workspace");
    let workspace_uri = url::Url::from_directory_path(&workspace)
        .unwrap()
        .to_string();
    write_transcript_named(
        home,
        session_id,
        "transcript_full.jsonl",
        &fixture
            .replace("__HOME_URI__", home_uri.trim_end_matches('/'))
            .replace("__WORKSPACE__", &workspace.display().to_string())
            .replace("__WORKSPACE_URI__", workspace_uri.trim_end_matches('/')),
    )
}
fn rewrite_subagent_result(fixture: &str, rewrite: impl FnOnce(&str) -> String) -> String {
    let mut records = fixture
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let result = records
        .iter_mut()
        .find(|record| record.get("type").and_then(Value::as_str) == Some("INVOKE_SUBAGENT"))
        .unwrap();
    let content = result.get("content").and_then(Value::as_str).unwrap();
    result["content"] = Value::String(rewrite(content));
    records
        .into_iter()
        .map(|record| record.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
fn state_after(home: &Path, workspace: &Path, ending: Vec<String>) -> LocalSessionState {
    let mut lines = vec![user_record(0, AT_09, "start")];
    lines.extend(ending);
    write_transcript_named(home, SESSION_ID, "transcript_full.jsonl", &lines.join("\n"));
    let observation = session::discover_under(home, workspace).remove(0);
    local_state(&observation).clone()
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
fn record(
    step: u64,
    source: &str,
    record_type: &str,
    status: &str,
    at: &str,
    content: &str,
) -> Value {
    json!({
        "step_index": step,
        "source": source,
        "type": record_type,
        "status": status,
        "created_at": at,
        "content": content,
    })
}
fn user_record(step: u64, at: &str, content: &str) -> String {
    record(step, "USER_EXPLICIT", "USER_INPUT", "DONE", at, content).to_string()
}
fn planner_record(step: u64, at: &str, tool_calls: Option<Value>) -> String {
    let mut value = record(
        step,
        "MODEL",
        "PLANNER_RESPONSE",
        "DONE",
        at,
        "planner response",
    );
    if let Some(tool_calls) = tool_calls {
        value["tool_calls"] = tool_calls;
    }
    value.to_string()
}
fn with<const N: usize>(base: &Value, fields: [(&str, Value); N]) -> Value {
    let mut value = base.clone();
    let object = value.as_object_mut().unwrap();
    for (key, field) in fields {
        object.insert(key.to_owned(), field);
    }
    value
}
